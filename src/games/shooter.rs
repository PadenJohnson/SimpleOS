//! Starbase Breach: an original first-person shooter. Fight through three decks of a
//! captured space station (Docking Bay, Reactor Deck, Command Core): keycard doors, secret
//! walls, three weapons, four kinds of robots and the Warden at the end.
//!
//! A raycaster drawn at a low internal resolution and scaled up. Per-pixel work is integer;
//! floats are only used per column / per row / per object.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::{rand_below, GameAction, Sfx, Sound};
use crate::gfx::{rgb, Canvas, Rect, Size};

const TEX: usize = 64;
const IW: usize = 320;

// ---- levels ----------------------------------------------------------------------------------
// Walls: # metal, % tech, = computer bank. D door, R red door, B blue door, S secret wall,
// X exit switch. @ start (facing east). Robots: d Seeker drone, c Skitter, b Hulk, W Warden.
// Items: h health, H big health, a armour, s shells, e cells, r red key, u blue key,
// G scattergun, P pulse rifle. Things: o barrel, l light pillar.

const LEVELS: [(&str, [&str; 22]); 3] = [
    (
        "Deck 1: Docking Bay",
        [
            "############################",
            "#@..#......#.......#.......#",
            "#...D..d...D...h...#...c...#",
            "#...#......#.......D.......#",
            "##D##..o...####D####...e...#",
            "#...#......#.......#.......#",
            "#.h.#......#...d...####R####",
            "#...####S###.......#.......#",
            "#...#...a..#..s....#...b...#",
            "#...#......####D####.......#",
            "#...#......#.......#..X....#",
            "#...D..c...D...c...#.......#",
            "#...#......#.......#.......#",
            "#####......#.G.....#########",
            "#...#......#.......#.......#",
            "#.r.D...d..#####S###...d...#",
            "#...#......#...H...#...s...#",
            "#####......#.......#.......#",
            "#...#....o.#..a....#.......#",
            "#.h.D......#.......D...c...#",
            "#...#......#.......#..P....#",
            "############################",
        ],
    ),
    (
        "Deck 2: Reactor Deck",
        [
            "############################",
            "#@.....%.......%.....s.....#",
            "#......%...d...%...........#",
            "#......D.......D.....c.....#",
            "#......%...h...%...........#",
            "###D####%%%D%%%%####D#######",
            "#......#.......#.....#.....#",
            "#..c...#.l...l.#..d..#..e..#",
            "#......D.......D.....R..u..#",
            "#..a...#.l...l.#..d..#.....#",
            "#......#.......#.....#..b..#",
            "###D####%%%D%%%#######B#####",
            "#..............#...........#",
            "#...o....d.....#...c.......#",
            "#..............#...........#",
            "#...####D####..#....H......#",
            "#...#..s..e.#..#...........#",
            "#...#..r.c..#..#...b...X...#",
            "#...#.......#..#...........#",
            "#...#########..#..d........#",
            "#..h...........#.......a...#",
            "############################",
        ],
    ),
    (
        "Deck 3: Command Core",
        [
            "############################",
            "#@..=.......=.......=.....h#",
            "#...=..d....D...c...=...d..#",
            "#...D.......=.......D......#",
            "#...=..s....=...a...=..b...#",
            "#...=====D============D#####",
            "#..........................#",
            "#..c.......o..........c....#",
            "#====D======D=====D=====R==#",
            "#.........=....=.....=.....#",
            "#..d...h..=..e.=..c..=..u..#",
            "#.........=....=..r..=.....#",
            "#..s......=.a..=.....=..b..#",
            "########################B###",
            "#..........................#",
            "#...l......l......l......l.#",
            "#..........................#",
            "#.....H.......W.......H....#",
            "#..........................#",
            "#...l......l..X...l......l.#",
            "#.......a.........e........#",
            "############################",
        ],
    ),
];

// Map cells.
const EMPTY: u8 = 0;
const WALL0: u8 = 1; // 1..=3 plain walls
const DOOR: u8 = 10;
const DOOR_RED: u8 = 11;
const DOOR_BLUE: u8 = 12;
const SECRET: u8 = 13;
const EXIT: u8 = 14;

fn is_door(c: u8) -> bool {
    (DOOR..=DOOR_BLUE).contains(&c)
}

// ---- pictures ----------------------------------------------------------------------------------

/// A small picture (0 = transparent).
struct Img {
    w: usize,
    h: usize,
    px: Vec<u32>,
}

impl Img {
    fn new(w: usize, h: usize) -> Img {
        Img { w, h, px: vec![0; w * h] }
    }
    fn set(&mut self, x: i32, y: i32, c: u32) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            self.px[y as usize * self.w + x as usize] = c | 0xFF00_0000;
        }
    }
    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: u32) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.set(xx, yy, c);
            }
        }
    }
    fn disc(&mut self, cx: i32, cy: i32, r: i32, c: u32) {
        for y in -r..=r {
            for x in -r..=r {
                if x * x + y * y <= r * r {
                    self.set(cx + x, cy + y, c);
                }
            }
        }
    }
    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, t: i32, c: u32) {
        let n = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
        for k in 0..=n {
            let x = x0 + (x1 - x0) * k / n;
            let y = y0 + (y1 - y0) * k / n;
            self.rect(x - t / 2, y - t / 2, t.max(1), t.max(1), c);
        }
    }
}

fn sq(v: f32) -> f32 {
    v * v
}

fn shade(c: u32, k: u32) -> u32 {
    let ch = |s: u32| (((c >> s) & 0xFF) * k >> 8) << s;
    ch(16) | ch(8) | ch(0)
}

fn noise(seed: &mut u32) -> u32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    *seed
}

/// Wall textures: 0 metal, 1 tech, 2 computer bank, 3 door, 4 red door, 5 blue door, 6 exit.
fn make_textures() -> Vec<Img> {
    let mut seed = 0x51A7u32;
    let mut out = Vec::new();
    let base = |seed: &mut u32, c: (i32, i32, i32), var: i32| {
        let mut t = Img::new(TEX, TEX);
        for i in 0..TEX * TEX {
            let n = (noise(seed) % (var as u32 * 2 + 1)) as i32 - var;
            t.px[i] = 0xFF00_0000 | (((c.0 + n).clamp(0, 255) as u32) << 16) | (((c.1 + n).clamp(0, 255) as u32) << 8) | (c.2 + n).clamp(0, 255) as u32;
        }
        t
    };
    // Metal panels with seams and rivets.
    let mut t = base(&mut seed, (88, 96, 110), 6);
    for k in [0, 31, 32, 63] {
        t.rect(0, k, 64, 1, rgb(50, 56, 66));
        t.rect(k, 0, 1, 64, rgb(50, 56, 66));
    }
    for (x, y) in [(4, 4), (27, 4), (36, 4), (59, 4), (4, 27), (27, 27), (36, 36), (59, 59), (4, 59), (27, 59), (36, 59), (59, 36)] {
        t.disc(x, y, 1, rgb(150, 156, 168));
    }
    out.push(t);
    // Tech: dark teal with glowing strips.
    let mut t = base(&mut seed, (34, 58, 66), 4);
    for y in [10, 30, 50] {
        t.rect(6, y, 52, 3, rgb(90, 230, 240));
        t.rect(6, y + 3, 52, 1, rgb(30, 110, 120));
    }
    t.rect(0, 0, 2, 64, rgb(20, 30, 36));
    t.rect(62, 0, 2, 64, rgb(20, 30, 36));
    out.push(t);
    // Computer bank: screens and lights.
    let mut t = base(&mut seed, (40, 40, 52), 3);
    t.rect(6, 6, 52, 22, rgb(20, 60, 40));
    for y in (9..26).step_by(4) {
        t.rect(9, y, 10 + (noise(&mut seed) % 30) as i32, 1, rgb(110, 240, 140));
    }
    for k in 0..24 {
        let col = [rgb(240, 80, 70), rgb(250, 210, 70), rgb(80, 200, 250), rgb(90, 240, 120)][(noise(&mut seed) % 4) as usize];
        t.rect(8 + (k % 8) * 6, 36 + (k / 8) * 7, 3, 3, col);
    }
    t.rect(0, 60, 64, 4, rgb(24, 24, 32));
    out.push(t);
    // Doors: steel with a coloured band.
    for band in [rgb(240, 190, 40), rgb(220, 40, 40), rgb(40, 110, 240)] {
        let mut t = base(&mut seed, (120, 124, 132), 5);
        t.rect(0, 26, 64, 12, band);
        for x in (0..64).step_by(8) {
            t.rect(x, 26, 4, 12, shade(band, 150));
        }
        t.rect(31, 0, 2, 64, rgb(70, 74, 80));
        t.rect(0, 0, 64, 2, rgb(70, 74, 80));
        out.push(t);
    }
    // Exit switch: panel with a big green arrow.
    let mut t = base(&mut seed, (60, 66, 76), 4);
    t.rect(12, 10, 40, 44, rgb(20, 40, 24));
    for k in 0..14 {
        t.rect(22 + k, 18 + k, 2, 28 - 2 * k, rgb(80, 240, 110));
    }
    t.rect(16, 28, 8, 8, rgb(80, 240, 110));
    out.push(t);
    out
}

fn make_flat(floor: bool) -> Img {
    let mut seed = if floor { 77 } else { 99 };
    let mut t = Img::new(TEX, TEX);
    for i in 0..TEX * TEX {
        let n = (noise(&mut seed) % 9) as i32 - 4;
        let (r, g, b) = if floor { (52 + n, 54 + n, 60 + n) } else { (30 + n, 32 + n, 40 + n) };
        t.px[i] = 0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32;
    }
    if floor {
        t.rect(0, 0, 64, 1, rgb(30, 32, 36));
        t.rect(0, 0, 1, 64, rgb(30, 32, 36));
        t.rect(0, 32, 64, 1, rgb(38, 40, 44));
        t.rect(32, 0, 1, 64, rgb(38, 40, 44));
    } else {
        t.rect(16, 16, 32, 32, rgb(170, 180, 190));
        t.rect(18, 18, 28, 28, rgb(230, 236, 240));
    }
    t
}

// Sprite numbers.
const SP_DRONE: usize = 0;
const SP_CRAWLER: usize = 1;
const SP_HULK: usize = 2;
const SP_WARDEN: usize = 3;
const SP_DEBRIS: usize = 4;
const SP_HEALTH: usize = 5;
const SP_BIGHEALTH: usize = 6;
const SP_ARMOUR: usize = 7;
const SP_SHELLS: usize = 8;
const SP_CELLS: usize = 9;
const SP_REDKEY: usize = 10;
const SP_BLUEKEY: usize = 11;
const SP_SCATTER: usize = 12;
const SP_PULSE: usize = 13;
const SP_BARREL: usize = 14;
const SP_LAMP: usize = 15;
const SP_BOLT: usize = 16;
const SP_BLAST: usize = 17;

fn make_sprites() -> Vec<Img> {
    let mut v = Vec::new();
    // Seeker drone: a sphere with fins and a red eye.
    let mut s = Img::new(64, 64);
    s.rect(6, 28, 52, 6, rgb(80, 90, 110));
    s.disc(32, 31, 18, rgb(150, 160, 180));
    s.disc(28, 26, 8, rgb(200, 210, 225));
    s.disc(32, 33, 7, rgb(40, 0, 0));
    s.disc(32, 33, 4, rgb(255, 60, 40));
    v.push(s);
    // Skitter: a low body on six legs.
    let mut s = Img::new(64, 64);
    for k in 0..3 {
        let x = 16 + k * 14;
        s.line(x, 46, x - 10, 62, 3, rgb(60, 70, 60));
        s.line(x + 4, 46, x + 14, 62, 3, rgb(60, 70, 60));
    }
    s.disc(32, 46, 14, rgb(90, 120, 80));
    s.rect(18, 46, 28, 8, rgb(70, 96, 64));
    s.disc(26, 42, 3, rgb(160, 255, 120));
    s.disc(38, 42, 3, rgb(160, 255, 120));
    v.push(s);
    // Hulk: a bulky mech with an arm cannon.
    let mut s = Img::new(64, 64);
    s.rect(20, 18, 24, 28, rgb(130, 100, 70));
    s.rect(24, 6, 16, 12, rgb(110, 84, 60));
    s.rect(27, 10, 10, 3, rgb(255, 200, 60));
    s.rect(10, 20, 10, 22, rgb(100, 80, 60));
    s.rect(44, 22, 14, 8, rgb(70, 70, 80));
    s.rect(56, 24, 6, 4, rgb(255, 120, 40));
    s.rect(22, 46, 8, 18, rgb(90, 70, 50));
    s.rect(34, 46, 8, 18, rgb(90, 70, 50));
    v.push(s);
    // Warden: a towering guardian with three eyes.
    let mut s = Img::new(64, 64);
    s.rect(14, 14, 36, 36, rgb(90, 50, 130));
    s.rect(8, 22, 8, 26, rgb(200, 160, 60));
    s.rect(48, 22, 8, 26, rgb(200, 160, 60));
    s.disc(32, 12, 12, rgb(120, 70, 170));
    for x in [24, 32, 40] {
        s.disc(x, 12, 3, rgb(255, 230, 90));
    }
    s.rect(20, 50, 10, 14, rgb(70, 40, 100));
    s.rect(34, 50, 10, 14, rgb(70, 40, 100));
    s.rect(24, 26, 16, 4, rgb(255, 230, 90));
    v.push(s);
    // Debris.
    let mut s = Img::new(64, 64);
    let mut seed = 5u32;
    for _ in 0..30 {
        let (x, y) = (8 + (noise(&mut seed) % 48) as i32, 52 + (noise(&mut seed) % 11) as i32);
        s.rect(x, y, 2 + (noise(&mut seed) % 6) as i32, 2 + (noise(&mut seed) % 3) as i32, [rgb(70, 70, 80), rgb(110, 110, 120), rgb(255, 140, 40)][(noise(&mut seed) % 3) as usize]);
    }
    v.push(s);
    // Health (green cross box), big health.
    for big in [false, true] {
        let mut s = Img::new(64, 64);
        let (w, h) = if big { (34, 22) } else { (24, 16) };
        let (x, y) = (32 - w / 2, 63 - h);
        s.rect(x, y, w, h, rgb(235, 240, 235));
        s.rect(30, y + 3, 4, h - 6, rgb(40, 180, 70));
        s.rect(32 - (h - 6) / 2, y + h / 2 - 2, h - 6, 4, rgb(40, 180, 70));
        v.push(s);
    }
    // Armour: a blue shield.
    let mut s = Img::new(64, 64);
    s.rect(22, 40, 20, 14, rgb(60, 120, 220));
    for k in 0..10 {
        s.rect(22 + k, 54 + k / 2, 20 - 2 * k, 1, rgb(60, 120, 220));
    }
    s.rect(30, 42, 4, 14, rgb(170, 210, 255));
    v.push(s);
    // Shells, cells.
    let mut s = Img::new(64, 64);
    s.rect(20, 50, 24, 13, rgb(140, 90, 40));
    for x in [23, 29, 35] {
        s.rect(x, 44, 4, 8, rgb(220, 60, 40));
        s.rect(x, 44, 4, 2, rgb(230, 200, 80));
    }
    v.push(s);
    let mut s = Img::new(64, 64);
    s.rect(24, 44, 16, 19, rgb(40, 80, 160));
    s.rect(28, 41, 8, 3, rgb(160, 170, 180));
    s.rect(27, 48, 10, 10, rgb(110, 230, 255));
    v.push(s);
    // Keycards.
    for col in [rgb(220, 40, 40), rgb(40, 110, 240)] {
        let mut s = Img::new(64, 64);
        s.rect(24, 48, 16, 12, col);
        s.rect(26, 50, 6, 3, rgb(240, 240, 240));
        s.rect(26, 55, 12, 2, shade(col, 140));
        v.push(s);
    }
    // Weapon pickups.
    let mut s = Img::new(64, 64);
    s.rect(14, 52, 34, 5, rgb(90, 90, 100));
    s.rect(14, 57, 10, 5, rgb(120, 80, 50));
    s.rect(44, 51, 6, 7, rgb(60, 60, 70));
    v.push(s);
    let mut s = Img::new(64, 64);
    s.rect(12, 50, 40, 7, rgb(60, 80, 110));
    s.rect(14, 57, 8, 6, rgb(50, 60, 80));
    s.rect(50, 52, 6, 3, rgb(110, 230, 255));
    s.rect(24, 47, 14, 3, rgb(110, 230, 255));
    v.push(s);
    // Barrel.
    let mut s = Img::new(64, 64);
    s.rect(20, 30, 24, 34, rgb(220, 120, 30));
    s.rect(20, 36, 24, 3, rgb(150, 80, 20));
    s.rect(20, 56, 24, 3, rgb(150, 80, 20));
    s.disc(32, 47, 5, rgb(30, 30, 30));
    s.disc(32, 47, 2, rgb(250, 220, 60));
    v.push(s);
    // Light pillar.
    let mut s = Img::new(64, 64);
    s.rect(26, 4, 12, 60, rgb(70, 76, 86));
    s.rect(28, 8, 8, 44, rgb(200, 240, 255));
    s.rect(24, 58, 16, 6, rgb(50, 54, 62));
    v.push(s);
    // Enemy bolt, explosion.
    let mut s = Img::new(64, 64);
    s.disc(32, 32, 9, rgb(255, 90, 50));
    s.disc(32, 32, 5, rgb(255, 230, 160));
    v.push(s);
    let mut s = Img::new(64, 64);
    s.disc(32, 34, 26, rgb(255, 120, 30));
    s.disc(32, 34, 17, rgb(255, 200, 60));
    s.disc(32, 34, 8, rgb(255, 250, 220));
    v.push(s);
    v
}

// ---- the game ----------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Title,
    Playing,
    Paused,
    LevelDone,
    Dead,
    Won,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Drone,
    Crawler,
    Hulk,
    Warden,
}

#[derive(Clone)]
struct Enemy {
    kind: Kind,
    x: f32,
    y: f32,
    hp: i32,
    awake: bool,
    cool: i32,
    pain: i32,
    flash: i32,
    dead: i32,
    sight: i32,
    sees: bool,
}

#[derive(Clone)]
struct Thing {
    x: f32,
    y: f32,
    sprite: usize,
    taken: bool,
    solid: bool,
    hp: i32,
}

#[derive(Clone)]
struct Bolt {
    x: f32,
    y: f32,
    dx: f32,
    dy: f32,
    dmg: i32,
    life: i32,
}

#[derive(Clone)]
struct Door {
    x: usize,
    y: usize,
    open: f32,
    dir: i8,
    timer: i32,
}

/// What the player carries from deck to deck.
#[derive(Clone)]
struct Kit {
    health: i32,
    armour: i32,
    shells: i32,
    cells: i32,
    have: [bool; 3],
    weapon: usize,
}

const WEAPONS: [&str; 3] = ["Blaster", "Scattergun", "Pulse rifle"];
const DIFFICULTY: [(&str, f32); 3] = [("Easy", 0.5), ("Normal", 1.0), ("Hard", 1.5)];

pub struct Shooter {
    state: State,
    level: usize,
    pub best_level: usize,
    difficulty: usize,
    map: Vec<u8>,
    mw: usize,
    mh: usize,
    seen: Vec<bool>,
    doors: Vec<Door>,
    x: f32,
    y: f32,
    angle: f32,
    kit: Kit,
    kit_at_start: Kit,
    keys: [bool; 2],
    enemies: Vec<Enemy>,
    things: Vec<Thing>,
    bolts: Vec<Bolt>,
    blasts: Vec<(f32, f32, i32)>,
    held: u32,
    cool: i32,
    muzzle: i32,
    hurt: i32,
    glow: i32,
    bob: f32,
    kills: (u32, u32),
    items: (u32, u32),
    secrets: (u32, u32),
    start_tick: u64,
    end_secs: u32,
    now: u64,
    message: Option<(String, u64)>,
    show_map: bool,
    textures: Vec<Img>,
    floor: Img,
    ceiling: Img,
    sprites: Vec<Img>,
    frame: Vec<u32>,
    zbuf: Vec<f32>,
    ih: usize,
    ticks: u64,
}

// Held keys.
const K_FWD: u32 = 1;
const K_BACK: u32 = 2;
const K_LEFT: u32 = 4;
const K_RIGHT: u32 = 8;
const K_SLEFT: u32 = 16;
const K_SRIGHT: u32 = 32;
const K_FIRE: u32 = 64;
const K_RUN: u32 = 128;

impl Shooter {
    pub fn new() -> Shooter {
        let kit = Kit { health: 100, armour: 0, shells: 0, cells: 0, have: [true, false, false], weapon: 0 };
        let mut s = Shooter {
            state: State::Title,
            level: 0,
            best_level: 0,
            difficulty: 1,
            map: Vec::new(),
            mw: 0,
            mh: 0,
            seen: Vec::new(),
            doors: Vec::new(),
            x: 0.0,
            y: 0.0,
            angle: 0.0,
            kit: kit.clone(),
            kit_at_start: kit,
            keys: [false; 2],
            enemies: Vec::new(),
            things: Vec::new(),
            bolts: Vec::new(),
            blasts: Vec::new(),
            held: 0,
            cool: 0,
            muzzle: 0,
            hurt: 0,
            glow: 0,
            bob: 0.0,
            kills: (0, 0),
            items: (0, 0),
            secrets: (0, 0),
            start_tick: 0,
            end_secs: 0,
            now: 0,
            message: None,
            show_map: false,
            textures: make_textures(),
            floor: make_flat(true),
            ceiling: make_flat(false),
            sprites: make_sprites(),
            frame: Vec::new(),
            zbuf: Vec::new(),
            ih: 0,
            ticks: 0,
        };
        s.load(0);
        s
    }

    fn load(&mut self, level: usize) {
        self.level = level;
        let rows = &LEVELS[level].1;
        self.mh = rows.len();
        self.mw = rows[0].len();
        self.map = vec![EMPTY; self.mw * self.mh];
        self.seen = vec![false; self.mw * self.mh];
        self.doors.clear();
        self.enemies.clear();
        self.things.clear();
        self.bolts.clear();
        self.blasts.clear();
        self.keys = [false; 2];
        self.kills = (0, 0);
        self.items = (0, 0);
        self.secrets = (0, 0);
        let hp_scale = [0.7f32, 1.0, 1.3][self.difficulty];
        for (y, row) in rows.iter().enumerate() {
            for (x, ch) in row.bytes().enumerate() {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let cell = match ch {
                    b'#' => WALL0,
                    b'%' => WALL0 + 1,
                    b'=' => WALL0 + 2,
                    b'D' => DOOR,
                    b'R' => DOOR_RED,
                    b'B' => DOOR_BLUE,
                    b'S' => SECRET,
                    b'X' => EXIT,
                    _ => EMPTY,
                };
                self.map[y * self.mw + x] = cell;
                if is_door(cell) {
                    self.doors.push(Door { x, y, open: 0.0, dir: 0, timer: 0 });
                }
                if cell == SECRET {
                    self.secrets.1 += 1;
                }
                let enemy = |kind: Kind, hp: i32| Enemy { kind, x: fx, y: fy, hp: (hp as f32 * hp_scale) as i32, awake: false, cool: 40 + rand_below(60) as i32, pain: 0, flash: 0, dead: 0, sight: 0, sees: false };
                let thing = |sprite: usize, solid: bool| Thing { x: fx, y: fy, sprite, taken: false, solid, hp: 30 };
                match ch {
                    b'@' => {
                        self.x = fx;
                        self.y = fy;
                        self.angle = 0.0;
                    }
                    b'd' => self.enemies.push(enemy(Kind::Drone, 30)),
                    b'c' => self.enemies.push(enemy(Kind::Crawler, 40)),
                    b'b' => self.enemies.push(enemy(Kind::Hulk, 160)),
                    b'W' => self.enemies.push(enemy(Kind::Warden, 900)),
                    b'h' => self.things.push(thing(SP_HEALTH, false)),
                    b'H' => self.things.push(thing(SP_BIGHEALTH, false)),
                    b'a' => self.things.push(thing(SP_ARMOUR, false)),
                    b's' => self.things.push(thing(SP_SHELLS, false)),
                    b'e' => self.things.push(thing(SP_CELLS, false)),
                    b'r' => self.things.push(thing(SP_REDKEY, false)),
                    b'u' => self.things.push(thing(SP_BLUEKEY, false)),
                    b'G' => self.things.push(thing(SP_SCATTER, false)),
                    b'P' => self.things.push(thing(SP_PULSE, false)),
                    b'o' => self.things.push(thing(SP_BARREL, true)),
                    b'l' => self.things.push(thing(SP_LAMP, true)),
                    _ => {}
                }
            }
        }
        self.kills.1 = self.enemies.len() as u32;
        self.items.1 = self.things.iter().filter(|t| !t.solid).count() as u32;
        self.kit_at_start = self.kit.clone();
        self.start_tick = self.now;
        self.held = 0;
        self.say(LEVELS[level].0);
    }

    fn say(&mut self, s: &str) {
        self.message = Some((String::from(s), self.now + 300));
    }

    fn new_game(&mut self, from_level: usize) {
        self.kit = Kit { health: 100, armour: 0, shells: 0, cells: 0, have: [true, false, false], weapon: 0 };
        if from_level >= 1 {
            // Starting on a later deck: give the weapons you'd have by then.
            self.kit.have = [true, true, from_level >= 2];
            self.kit.shells = 20;
            self.kit.cells = if from_level >= 2 { 60 } else { 0 };
        }
        self.load(from_level);
        self.state = State::Playing;
    }

    // ---- map ---------------------------------------------------------------------------------

    fn cell(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x as usize >= self.mw || y as usize >= self.mh {
            return WALL0;
        }
        self.map[y as usize * self.mw + x as usize]
    }

    fn door_open(&self, x: i32, y: i32) -> f32 {
        self.doors.iter().find(|d| d.x as i32 == x && d.y as i32 == y).map(|d| d.open).unwrap_or(0.0)
    }

    /// Can something stand at (x, y)?
    fn blocked(&self, x: f32, y: f32) -> bool {
        let c = self.cell(x as i32, y as i32);
        if c == EMPTY {
            return false;
        }
        if is_door(c) {
            return self.door_open(x as i32, y as i32) < 0.85;
        }
        true
    }

    fn solid_thing_near(&self, x: f32, y: f32, r: f32) -> bool {
        self.things.iter().any(|t| t.solid && !t.taken && (t.x - x).abs() < r + 0.25 && (t.y - y).abs() < r + 0.25)
    }

    /// Move with sliding along walls.
    fn try_move(&self, x: f32, y: f32, dx: f32, dy: f32, r: f32) -> (f32, f32) {
        let free = |px: f32, py: f32| !(self.blocked(px - r, py - r) || self.blocked(px + r, py - r) || self.blocked(px - r, py + r) || self.blocked(px + r, py + r) || self.solid_thing_near(px, py, r));
        let mut nx = x;
        let mut ny = y;
        if free(x + dx, y) {
            nx = x + dx;
        }
        if free(nx, y + dy) {
            ny = y + dy;
        }
        (nx, ny)
    }

    /// Nothing solid between two points?
    fn clear_line(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len = libm::sqrtf(dx * dx + dy * dy);
        let n = (len / 0.2) as i32 + 1;
        for k in 1..n {
            let t = k as f32 / n as f32;
            if self.blocked(x0 + dx * t, y0 + dy * t) {
                return false;
            }
        }
        true
    }

    /// Distance along a ray to the first wall (doors count when mostly closed).
    fn wall_distance(&self, x: f32, y: f32, a: f32) -> f32 {
        let (dx, dy) = (libm::cosf(a), libm::sinf(a));
        let mut d = 0.0;
        while d < 40.0 {
            d += 0.05;
            if self.blocked(x + dx * d, y + dy * d) {
                return d;
            }
        }
        d
    }

    // ---- playing -----------------------------------------------------------------------------

    fn hurt_player(&mut self, dmg: i32, sfx: &mut Sfx) {
        let dmg = (dmg as f32 * DIFFICULTY[self.difficulty].1) as i32;
        let saved = (dmg / 3).min(self.kit.armour);
        self.kit.armour -= saved;
        self.kit.health -= dmg - saved;
        self.hurt = 20;
        sfx.play(Sound::Bump);
        if self.kit.health <= 0 {
            self.kit.health = 0;
            self.state = State::Dead;
            sfx.play(Sound::Lost);
        }
    }

    fn fire(&mut self, sfx: &mut Sfx) {
        if self.cool > 0 {
            return;
        }
        let w = self.kit.weapon;
        let (pellets, dmg, spread, cool) = match w {
            1 if self.kit.shells > 0 => {
                self.kit.shells -= 1;
                (7, 9, 0.11, 75)
            }
            2 if self.kit.cells > 0 => {
                self.kit.cells -= 1;
                (1, 11, 0.02, 10)
            }
            0 => (1, 14, 0.015, 32),
            _ => {
                // Out of ammo: back to the blaster.
                self.kit.weapon = 0;
                self.say("Out of ammo");
                return;
            }
        };
        self.cool = cool;
        self.muzzle = 8;
        sfx.play(if w == 1 { Sound::Boom } else { Sound::Click });
        for _ in 0..pellets {
            let a = self.angle + (rand_below(1000) as f32 / 1000.0 - 0.5) * 2.0 * spread;
            self.hitscan(a, dmg + rand_below(5) as i32, sfx);
        }
        // Shots wake robots nearby.
        for e in self.enemies.iter_mut() {
            if e.dead == 0 && (e.x - self.x).abs() + (e.y - self.y).abs() < 9.0 {
                e.awake = true;
            }
        }
    }

    fn hitscan(&mut self, a: f32, dmg: i32, sfx: &mut Sfx) {
        let wall = self.wall_distance(self.x, self.y, a);
        let (dx, dy) = (libm::cosf(a), libm::sinf(a));
        // The nearest robot or barrel the ray passes through.
        let mut best: Option<(f32, bool, usize)> = None;
        for (i, e) in self.enemies.iter().enumerate() {
            if e.dead > 0 {
                continue;
            }
            let r = match e.kind {
                Kind::Hulk => 0.45,
                Kind::Warden => 0.6,
                _ => 0.35,
            };
            let (ex, ey) = (e.x - self.x, e.y - self.y);
            let along = ex * dx + ey * dy;
            let off = (ex * dy - ey * dx).abs();
            if along > 0.0 && along < wall && off < r && best.is_none_or(|b| along < b.0) {
                best = Some((along, true, i));
            }
        }
        for (i, t) in self.things.iter().enumerate() {
            if t.sprite != SP_BARREL || t.taken {
                continue;
            }
            let (ex, ey) = (t.x - self.x, t.y - self.y);
            let along = ex * dx + ey * dy;
            let off = (ex * dy - ey * dx).abs();
            if along > 0.0 && along < wall && off < 0.3 && best.is_none_or(|b| along < b.0) {
                best = Some((along, false, i));
            }
        }
        match best {
            Some((_, true, i)) => self.damage_enemy(i, dmg, sfx),
            Some((_, false, i)) => {
                self.things[i].hp -= dmg;
                if self.things[i].hp <= 0 {
                    self.explode(i, sfx);
                }
            }
            None => {}
        }
    }

    fn damage_enemy(&mut self, i: usize, dmg: i32, sfx: &mut Sfx) {
        let e = &mut self.enemies[i];
        if e.dead > 0 {
            return;
        }
        e.hp -= dmg;
        e.awake = true;
        e.flash = 6;
        if e.kind != Kind::Warden && rand_below(3) == 0 {
            e.pain = 15;
        }
        if e.hp <= 0 {
            e.dead = 1;
            self.kills.0 += 1;
            sfx.play(Sound::Drop);
            if e.kind == Kind::Hulk {
                let (x, y) = (e.x, e.y);
                self.things.push(Thing { x, y, sprite: SP_CELLS, taken: false, solid: false, hp: 0 });
            }
            if e.kind == Kind::Warden {
                self.say("The Warden is down! Find the exit switch");
                sfx.play(Sound::Win);
            }
        }
    }

    fn explode(&mut self, i: usize, sfx: &mut Sfx) {
        let (x, y) = (self.things[i].x, self.things[i].y);
        self.things[i].taken = true;
        self.blasts.push((x, y, 30));
        sfx.play(Sound::Boom);
        for k in 0..self.enemies.len() {
            let d = libm::sqrtf(sq(self.enemies[k].x - x) + sq(self.enemies[k].y - y));
            if d < 2.5 {
                self.damage_enemy(k, (90.0 * (1.0 - d / 2.5)) as i32 + 10, sfx);
            }
        }
        let d = libm::sqrtf(sq(self.x - x) + sq(self.y - y));
        if d < 2.5 {
            self.hurt_player((50.0 * (1.0 - d / 2.5)) as i32, sfx);
        }
        // Chain reactions.
        for k in 0..self.things.len() {
            if k != i && self.things[k].sprite == SP_BARREL && !self.things[k].taken {
                let d = libm::sqrtf(sq(self.things[k].x - x) + sq(self.things[k].y - y));
                if d < 2.0 {
                    self.things[k].hp = 0;
                    self.explode(k, sfx);
                }
            }
        }
    }

    /// Open doors, find secrets, press the exit.
    fn use_key(&mut self, sfx: &mut Sfx) {
        let (dx, dy) = (libm::cosf(self.angle), libm::sinf(self.angle));
        let mut d = 0.3;
        while d < 1.6 {
            let (cx, cy) = ((self.x + dx * d) as i32, (self.y + dy * d) as i32);
            let c = self.cell(cx, cy);
            if c != EMPTY {
                match c {
                    DOOR | DOOR_RED | DOOR_BLUE => {
                        if c == DOOR_RED && !self.keys[0] {
                            self.say("You need the red keycard");
                        } else if c == DOOR_BLUE && !self.keys[1] {
                            self.say("You need the blue keycard");
                        } else if let Some(door) = self.doors.iter_mut().find(|o| o.x as i32 == cx && o.y as i32 == cy) {
                            if door.dir <= 0 && door.open < 1.0 {
                                door.dir = 1;
                                sfx.play(Sound::Launch);
                            }
                            door.timer = 500;
                        }
                    }
                    SECRET => {
                        self.map[cy as usize * self.mw + cx as usize] = EMPTY;
                        self.secrets.0 += 1;
                        self.say("A secret passage!");
                        sfx.play(Sound::Win);
                    }
                    EXIT => {
                        if self.enemies.iter().any(|e| e.kind == Kind::Warden && e.dead == 0) {
                            self.say("The exit is locked while the Warden stands guard");
                        } else {
                            self.end_secs = ((self.now - self.start_tick) / 100) as u32;
                            self.state = if self.level + 1 >= LEVELS.len() { State::Won } else { State::LevelDone };
                            self.best_level = self.best_level.max((self.level + 1).min(LEVELS.len() - 1));
                            sfx.play(Sound::Win);
                        }
                    }
                    _ => {}
                }
                return;
            }
            d += 0.1;
        }
    }

    fn pick_up(&mut self, sfx: &mut Sfx) {
        for i in 0..self.things.len() {
            let t = &self.things[i];
            if t.taken || t.solid || (t.x - self.x).abs() > 0.5 || (t.y - self.y).abs() > 0.5 {
                continue;
            }
            let k = &mut self.kit;
            let (ok, msg) = match t.sprite {
                SP_HEALTH => (k.health < 100, "Repair kit"),
                SP_BIGHEALTH => (k.health < 150, "Big repair kit"),
                SP_ARMOUR => (k.armour < 100, "Armour"),
                SP_SHELLS => (true, "Shells"),
                SP_CELLS => (true, "Energy cells"),
                SP_REDKEY => (true, "Red keycard"),
                SP_BLUEKEY => (true, "Blue keycard"),
                SP_SCATTER => (true, "Scattergun! (press 2)"),
                SP_PULSE => (true, "Pulse rifle! (press 3)"),
                _ => (false, ""),
            };
            if !ok {
                continue;
            }
            match t.sprite {
                SP_HEALTH => k.health = (k.health + 25).min(100),
                SP_BIGHEALTH => k.health = (k.health + 50).min(150),
                SP_ARMOUR => k.armour = (k.armour + 50).min(100),
                SP_SHELLS => k.shells = (k.shells + 8).min(60),
                SP_CELLS => k.cells = (k.cells + 40).min(300),
                SP_REDKEY => self.keys[0] = true,
                SP_BLUEKEY => self.keys[1] = true,
                SP_SCATTER => {
                    k.have[1] = true;
                    k.shells = (k.shells + 10).min(60);
                    k.weapon = 1;
                }
                SP_PULSE => {
                    k.have[2] = true;
                    k.cells = (k.cells + 60).min(300);
                    k.weapon = 2;
                }
                _ => {}
            }
            self.things[i].taken = true;
            self.items.0 += 1;
            self.glow = 12;
            sfx.play(Sound::Click);
            self.say(msg);
        }
    }

    fn think(&mut self, sfx: &mut Sfx) {
        let (px, py) = (self.x, self.y);
        for i in 0..self.enemies.len() {
            let mut e = self.enemies[i].clone();
            if e.dead > 0 {
                e.dead += 1;
                self.enemies[i] = e;
                continue;
            }
            e.flash = (e.flash - 1).max(0);
            let (dx, dy) = (px - e.x, py - e.y);
            let dist = libm::sqrtf(dx * dx + dy * dy).max(0.01);
            e.sight -= 1;
            if e.sight <= 0 {
                e.sight = 12;
                e.sees = dist < 18.0 && self.clear_line(e.x, e.y, px, py);
                if e.sees && !e.awake {
                    e.awake = true;
                    sfx.play(Sound::Bump);
                }
            }
            if !e.awake {
                self.enemies[i] = e;
                continue;
            }
            if e.pain > 0 {
                e.pain -= 1;
                self.enemies[i] = e;
                continue;
            }
            let (speed, keep, range) = match e.kind {
                Kind::Drone => (1.9, 3.5, 12.0),
                Kind::Crawler => (2.6, 0.7, 0.9),
                Kind::Hulk => (1.1, 4.5, 14.0),
                Kind::Warden => (1.3, 5.0, 18.0),
            };
            if dist > keep || !e.sees {
                let step = speed * 0.01;
                let (mx, my) = self.try_move(e.x, e.y, dx / dist * step, dy / dist * step, 0.3);
                // Stuck against a wall: try sideways.
                let (mx, my) = if (mx - e.x).abs() + (my - e.y).abs() < step * 0.3 { self.try_move(e.x, e.y, -dy / dist * step, dx / dist * step, 0.3) } else { (mx, my) };
                e.x = mx;
                e.y = my;
            }
            e.cool -= 1;
            if e.cool <= 0 && e.sees && dist < range {
                match e.kind {
                    Kind::Crawler => {
                        e.cool = 60;
                        self.enemies[i] = e.clone();
                        self.hurt_player(8 + rand_below(5) as i32, sfx);
                        e = self.enemies[i].clone();
                    }
                    k => {
                        let (n, spread, dmg, spd, cool) = match k {
                            Kind::Drone => (1, 0.0, 6, 6.0, 110),
                            Kind::Hulk => (3, 0.18, 8, 7.5, 140),
                            _ => (5, 0.25, 10, 8.0, 70),
                        };
                        e.cool = cool + rand_below(40) as i32;
                        e.flash = 8;
                        let base = libm::atan2f(dy, dx);
                        for s in 0..n {
                            let a = base + (s as f32 - (n - 1) as f32 / 2.0) * spread + (rand_below(100) as f32 / 100.0 - 0.5) * 0.08;
                            self.bolts.push(Bolt { x: e.x, y: e.y, dx: libm::cosf(a) * spd * 0.01, dy: libm::sinf(a) * spd * 0.01, dmg, life: 600 });
                        }
                        sfx.play(Sound::Flip);
                    }
                }
            }
            self.enemies[i] = e;
        }
        // Bolts.
        let mut hits = 0;
        let mut i = 0;
        while i < self.bolts.len() {
            let b = &mut self.bolts[i];
            b.x += b.dx;
            b.y += b.dy;
            b.life -= 1;
            let (bx, by, dmg, life) = (b.x, b.y, b.dmg, b.life);
            let gone = if (bx - px).abs() < 0.35 && (by - py).abs() < 0.35 {
                hits += dmg;
                true
            } else {
                life <= 0 || self.blocked(bx, by)
            };
            if gone {
                self.bolts.remove(i);
            } else {
                i += 1;
            }
        }
        if hits > 0 {
            self.hurt_player(hits, sfx);
        }
        // Doors.
        let (pcx, pcy) = (px as usize, py as usize);
        let occupied: Vec<(usize, usize)> = self.enemies.iter().filter(|e| e.dead == 0).map(|e| (e.x as usize, e.y as usize)).chain(core::iter::once((pcx, pcy))).collect();
        for d in self.doors.iter_mut() {
            match d.dir {
                1 => {
                    d.open = (d.open + 0.02).min(1.0);
                    if d.open >= 1.0 {
                        d.dir = 0;
                    }
                }
                -1 => {
                    if occupied.contains(&(d.x, d.y)) {
                        d.dir = 1;
                    } else {
                        d.open = (d.open - 0.02).max(0.0);
                        if d.open <= 0.0 {
                            d.dir = 0;
                        }
                    }
                }
                _ => {
                    if d.open >= 1.0 {
                        d.timer -= 1;
                        if d.timer <= 0 && !occupied.contains(&(d.x, d.y)) {
                            d.dir = -1;
                        }
                    }
                }
            }
        }
        // Robots open doors in their way too.
        for e in self.enemies.iter() {
            if e.dead > 0 || !e.awake {
                continue;
            }
            for (ox, oy) in [(0.6f32, 0.0f32), (-0.6, 0.0), (0.0, 0.6), (0.0, -0.6)] {
                let (cx, cy) = ((e.x + ox) as usize, (e.y + oy) as usize);
                if let Some(d) = self.doors.iter_mut().find(|d| d.x == cx && d.y == cy) {
                    let c = self.map[cy * self.mw + cx];
                    if c == DOOR && d.dir <= 0 && d.open < 1.0 {
                        d.dir = 1;
                        d.timer = 300;
                    }
                }
            }
        }
        for b in self.blasts.iter_mut() {
            b.2 -= 1;
        }
        self.blasts.retain(|b| b.2 > 0);
    }

    fn step(&mut self, sfx: &mut Sfx) {
        let run = if self.held & K_RUN != 0 { 1.6 } else { 1.0 };
        let turn = 2.6 * 0.01 * if self.held & K_RUN != 0 { 1.3 } else { 1.0 };
        if self.held & K_LEFT != 0 {
            self.angle -= turn;
        }
        if self.held & K_RIGHT != 0 {
            self.angle += turn;
        }
        let (dx, dy) = (libm::cosf(self.angle), libm::sinf(self.angle));
        let mut mx = 0.0;
        let mut my = 0.0;
        let sp = 3.4 * 0.01 * run;
        if self.held & K_FWD != 0 {
            mx += dx * sp;
            my += dy * sp;
        }
        if self.held & K_BACK != 0 {
            mx -= dx * sp;
            my -= dy * sp;
        }
        if self.held & K_SLEFT != 0 {
            mx += dy * sp;
            my -= dx * sp;
        }
        if self.held & K_SRIGHT != 0 {
            mx -= dy * sp;
            my += dx * sp;
        }
        if mx != 0.0 || my != 0.0 {
            let (nx, ny) = self.try_move(self.x, self.y, mx, my, 0.25);
            self.x = nx;
            self.y = ny;
            self.bob += 0.18 * run;
        }
        self.cool = (self.cool - 1).max(0);
        self.muzzle = (self.muzzle - 1).max(0);
        self.hurt = (self.hurt - 1).max(0);
        self.glow = (self.glow - 1).max(0);
        if self.held & K_FIRE != 0 {
            self.fire(sfx);
        }
        self.pick_up(sfx);
        self.think(sfx);
        // Automap: mark what's around.
        let (cx, cy) = (self.x as i32, self.y as i32);
        for yy in cy - 3..=cy + 3 {
            for xx in cx - 3..=cx + 3 {
                if xx >= 0 && yy >= 0 && (xx as usize) < self.mw && (yy as usize) < self.mh {
                    self.seen[yy as usize * self.mw + xx as usize] = true;
                }
            }
        }
    }

    pub fn tick(&mut self, now: u64, focused: bool, sfx: &mut Sfx) -> bool {
        let elapsed = now.saturating_sub(self.now).min(4);
        self.now = now;
        if !focused {
            self.held = 0;
            return false;
        }
        if self.state != State::Playing {
            return self.ticks % 20 == 0;
        }
        for _ in 0..elapsed {
            self.step(sfx);
            if self.state != State::Playing {
                break;
            }
        }
        self.ticks += 1;
        self.ticks % 2 == 0
    }

    // ---- drawing -------------------------------------------------------------------------------

    fn render(&mut self) {
        let (w, h) = (IW, self.ih);
        let mut frame_buf = core::mem::take(&mut self.frame);
        let frame = &mut frame_buf;
        let (dirx, diry) = (libm::cosf(self.angle), libm::sinf(self.angle));
        let (plx, ply) = (-diry * 0.66, dirx * 0.66);
        let half = h / 2;
        // Floor and ceiling.
        for y in half + 1..h {
            let p = (y - half) as f32;
            let rowdist = half as f32 / p;
            let (r0x, r0y) = (dirx - plx, diry - ply);
            let (r1x, r1y) = (dirx + plx, diry + ply);
            let sx = (rowdist * (r1x - r0x) / w as f32 * 65536.0) as i64;
            let sy = (rowdist * (r1y - r0y) / w as f32 * 65536.0) as i64;
            let mut fx = ((self.x + rowdist * r0x) * 65536.0) as i64;
            let mut fy = ((self.y + rowdist * r0y) * 65536.0) as i64;
            let k = (255.0 - rowdist * 26.0).clamp(50.0, 255.0) as u32;
            let (frow, crow) = (y * w, (h - 1 - y) * w);
            for x in 0..w {
                let tx = ((fx >> 10) & 63) as usize;
                let ty = ((fy >> 10) & 63) as usize;
                frame[frow + x] = shade(self.floor.px[ty * 64 + tx], k);
                frame[crow + x] = shade(self.ceiling.px[ty * 64 + tx], k);
                fx += sx;
                fy += sy;
            }
        }
        // Walls.
        for x in 0..w {
            let cam = 2.0 * x as f32 / w as f32 - 1.0;
            let (rx, ry) = (dirx + plx * cam, diry + ply * cam);
            let (mut mapx, mut mapy) = (self.x as i32, self.y as i32);
            let ddx = if rx == 0.0 { 1e30 } else { (1.0 / rx).abs() };
            let ddy = if ry == 0.0 { 1e30 } else { (1.0 / ry).abs() };
            let (stepx, mut sidex) = if rx < 0.0 { (-1, (self.x - mapx as f32) * ddx) } else { (1, (mapx as f32 + 1.0 - self.x) * ddx) };
            let (stepy, mut sidey) = if ry < 0.0 { (-1, (self.y - mapy as f32) * ddy) } else { (1, (mapy as f32 + 1.0 - self.y) * ddy) };
            let mut dist = 30.0;
            let mut tex = 0usize;
            let mut texx = 0.0f32;
            let mut side = 0;
            for _ in 0..64 {
                if sidex < sidey {
                    sidex += ddx;
                    mapx += stepx;
                    side = 0;
                } else {
                    sidey += ddy;
                    mapy += stepy;
                    side = 1;
                }
                let c = self.cell(mapx, mapy);
                if c == EMPTY {
                    continue;
                }
                if is_door(c) {
                    // The door sits in the middle of its cell.
                    let (d, other) = if side == 0 { (sidex - ddx * 0.5, sidey) } else { (sidey - ddy * 0.5, sidex) };
                    if d > other {
                        continue;
                    }
                    let along = if side == 0 { self.y + d * ry } else { self.x + d * rx };
                    let frac = along - libm::floorf(along);
                    let open = self.door_open(mapx, mapy);
                    if frac < open {
                        continue;
                    }
                    dist = d;
                    texx = frac - open;
                    tex = 3 + (c - DOOR) as usize;
                    break;
                }
                dist = if side == 0 { sidex - ddx } else { sidey - ddy };
                let along = if side == 0 { self.y + dist * ry } else { self.x + dist * rx };
                texx = along - libm::floorf(along);
                tex = match c {
                    EXIT => 6,
                    SECRET => 0,
                    c => (c - WALL0) as usize,
                };
                break;
            }
            let dist = dist.max(0.05);
            self.zbuf[x] = dist;
            let lh = (h as f32 / dist) as i64;
            let top = half as i64 - lh / 2;
            let t = &self.textures[tex.min(self.textures.len() - 1)];
            let tx = ((texx * 64.0) as usize).min(63);
            let k = ((255.0 - dist * 24.0).clamp(50.0, 255.0) as u32) * if side == 1 { 200 } else { 256 } / 256;
            let step = (64 << 16) / lh.max(1);
            let y0 = top.max(0);
            let y1 = (top + lh).min(h as i64);
            let mut tpos = (y0 - top) * step;
            for y in y0..y1 {
                let ty = ((tpos >> 16) as usize).min(63);
                frame[y as usize * w + x] = shade(t.px[ty * 64 + tx], k);
                tpos += step;
            }
        }
        // Sprites, far to near.
        let mut list: Vec<(f32, f32, usize, f32, f32, u32)> = Vec::new(); // x, y, sprite, scale, lift, tint
        for e in &self.enemies {
            let (sp, scale, lift) = if e.dead > 0 {
                (SP_DEBRIS, 1.0, 0.0)
            } else {
                match e.kind {
                    Kind::Drone => (SP_DRONE, 0.55, 0.25 + 0.04 * libm::sinf(self.ticks as f32 * 0.1)),
                    Kind::Crawler => (SP_CRAWLER, 0.7, 0.0),
                    Kind::Hulk => (SP_HULK, 1.05, 0.0),
                    Kind::Warden => (SP_WARDEN, 1.5, 0.0),
                }
            };
            let tint = if e.dead == 0 && e.flash > 0 { 1 } else { 0 };
            list.push((e.x, e.y, sp, scale, lift, tint));
        }
        for t in &self.things {
            if !t.taken {
                let scale = if t.sprite == SP_LAMP { 1.0 } else { 0.7 };
                list.push((t.x, t.y, t.sprite, scale, 0.0, 0));
            }
        }
        for b in &self.bolts {
            list.push((b.x, b.y, SP_BOLT, 0.3, 0.35, 2));
        }
        for b in &self.blasts {
            list.push((b.0, b.1, SP_BLAST, 1.0, 0.0, 2));
        }
        list.sort_by(|a, b| {
            let da = sq(a.0 - self.x) + sq(a.1 - self.y);
            let db = sq(b.0 - self.x) + sq(b.1 - self.y);
            db.partial_cmp(&da).unwrap_or(core::cmp::Ordering::Equal)
        });
        let inv = 1.0 / (plx * diry - dirx * ply);
        for (sx, sy, sp, scale, lift, tint) in list {
            let (rx, ry) = (sx - self.x, sy - self.y);
            let tx = inv * (diry * rx - dirx * ry);
            let ty = inv * (-ply * rx + plx * ry);
            if ty <= 0.1 {
                continue;
            }
            let screen_x = (w as f32 / 2.0) * (1.0 + tx / ty);
            let full = h as f32 / ty;
            let size = (full * scale) as i64;
            if size <= 0 {
                continue;
            }
            let floor_y = half as f32 + full / 2.0;
            let bottom = (floor_y - lift * full) as i64;
            let top = bottom - size;
            let left = screen_x as i64 - size / 2;
            let img = &self.sprites[sp];
            let k = (255.0 - ty * 24.0).clamp(60.0, 255.0) as u32;
            let step = (64 << 16) / size;
            for x in left.max(0)..(left + size).min(w as i64) {
                if ty >= self.zbuf[x as usize] {
                    continue;
                }
                let u = (((x - left) * step) >> 16) as usize;
                let mut vpos = (top.max(0) - top) * step;
                for y in top.max(0)..bottom.min(h as i64) {
                    let v = ((vpos >> 16) as usize).min(63);
                    vpos += step;
                    let p = img.px[v * 64 + u.min(63)];
                    if p >> 24 == 0 {
                        continue;
                    }
                    let c = match tint {
                        1 => 0xFFFFFF,
                        2 => p & 0xFF_FFFF,
                        _ => shade(p & 0xFF_FFFF, k),
                    };
                    frame[y as usize * w + x as usize] = c;
                }
            }
        }
        // The weapon.
        let bob = libm::sinf(self.bob) * h as f32 * 0.02;
        let (gx, gy) = (w as i32 / 2 + (bob * 1.5) as i32, h as i32 - (h as i32 / 5) + bob.abs() as i32);
        let px = |frame: &mut Vec<u32>, x: i32, y: i32, ww: i32, hh: i32, c: u32| {
            for yy in y.max(0)..(y + hh).min(h as i32) {
                for xx in x.max(0)..(x + ww).min(w as i32) {
                    frame[yy as usize * w + xx as usize] = c;
                }
            }
        };
        let u = (h as i32 / 60).max(1);
        if self.muzzle > 0 {
            let c = if self.kit.weapon == 2 { rgb(120, 230, 255) } else { rgb(255, 220, 120) };
            px(frame, gx - 5 * u, gy - 12 * u, 10 * u, 8 * u, c);
        }
        match self.kit.weapon {
            0 => {
                px(frame, gx - 3 * u, gy - 6 * u, 6 * u, 18 * u, rgb(90, 96, 110));
                px(frame, gx - 2 * u, gy - 5 * u, 4 * u, 3 * u, rgb(255, 120, 60));
                px(frame, gx - 5 * u, gy + 6 * u, 10 * u, 12 * u, rgb(60, 64, 74));
            }
            1 => {
                px(frame, gx - 6 * u, gy - 6 * u, 5 * u, 22 * u, rgb(80, 80, 90));
                px(frame, gx + u, gy - 6 * u, 5 * u, 22 * u, rgb(80, 80, 90));
                px(frame, gx - 8 * u, gy + 8 * u, 16 * u, 12 * u, rgb(120, 80, 50));
            }
            _ => {
                px(frame, gx - 4 * u, gy - 8 * u, 8 * u, 24 * u, rgb(60, 80, 110));
                px(frame, gx - 3 * u, gy - 4 * u, 6 * u, 6 * u, rgb(110, 230, 255));
                px(frame, gx - 7 * u, gy + 8 * u, 14 * u, 12 * u, rgb(50, 60, 80));
            }
        }
        // Hurt / pick-up flashes.
        if self.hurt > 0 || self.glow > 0 {
            let (c, a) = if self.hurt > 0 { (rgb(255, 0, 0), self.hurt as u32 * 6) } else { (rgb(255, 230, 120), self.glow as u32 * 4) };
            for p in frame.iter_mut() {
                *p = crate::gfx::mix(*p, c, a.min(120));
            }
        }
        self.frame = frame_buf;
    }

    pub fn draw(&mut self, c: &mut Canvas, r: Rect) {
        c.fill_rect(r.x, r.y, r.w, r.h, 0);
        let hud = 64;
        let view = Rect::new(r.x, r.y, r.w, r.h - hud);
        let scale = ((view.w as usize) / IW).max(1);
        let ih = ((view.h as usize) / scale).max(60).min(400);
        if self.ih != ih {
            self.ih = ih;
            self.frame = vec![0; IW * ih];
            self.zbuf = vec![0.0; IW];
        }
        if matches!(self.state, State::Title) {
            self.draw_title(c, r);
            return;
        }
        self.render();
        // Scale up into the window.
        let ox = r.x + (view.w - (IW * scale) as i32) / 2;
        let oy = r.y + (view.h - (ih * scale) as i32) / 2;
        let cw = c.w as i32;
        for y in 0..ih {
            for s in 0..scale {
                let yy = oy + (y * scale + s) as i32;
                if yy < r.y || yy >= view.y + view.h || yy >= c.h as i32 {
                    continue;
                }
                let row = &self.frame[y * IW..(y + 1) * IW];
                let base = (yy * cw) as usize;
                for (x, &p) in row.iter().enumerate() {
                    let xx = ox + (x * scale) as i32;
                    if xx < 0 || xx + scale as i32 > cw {
                        continue;
                    }
                    let o = base + xx as usize;
                    c.px[o..o + scale].fill(p);
                }
            }
        }
        // Crosshair.
        let (cx, cy) = (ox + (IW * scale) as i32 / 2, oy + (ih * scale) as i32 / 2);
        c.fill_rect(cx - 8, cy, 5, 2, rgb(255, 255, 255));
        c.fill_rect(cx + 4, cy, 5, 2, rgb(255, 255, 255));
        c.fill_rect(cx, cy - 8, 2, 5, rgb(255, 255, 255));
        c.fill_rect(cx, cy + 4, 2, 5, rgb(255, 255, 255));
        if self.show_map {
            self.draw_map(c, view);
        }
        self.draw_hud(c, Rect::new(r.x, r.y + r.h - hud, r.w, hud));
        if let Some((m, until)) = &self.message {
            if self.now < *until {
                c.text_centered(r.x + r.w / 2, r.y + 14, m, Size::Medium, true, rgb(255, 230, 140));
            }
        }
        match self.state {
            State::Paused => super::banner(c, view, "Paused", "Press Esc to carry on (close the window to stop)"),
            State::Dead => super::banner(c, view, "You were overwhelmed", "Press Enter to try this deck again"),
            State::LevelDone => {
                let s = format!("Robots {}/{}  Items {}/{}  Secrets {}/{}  Time {}", self.kills.0, self.kills.1, self.items.0, self.items.1, self.secrets.0, self.secrets.1, super::time_text(self.end_secs));
                super::banner(c, view, "Deck cleared!", &s);
                c.text_centered(view.x + view.w / 2, view.y + view.h / 2 + 76, "Press Enter for the next deck", Size::Small, true, rgb(255, 255, 255));
            }
            State::Won => super::banner(c, view, "The station is yours!", "You beat Starbase Breach. Press Enter to play again"),
            _ => {}
        }
    }

    fn draw_hud(&self, c: &mut Canvas, r: Rect) {
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(26, 28, 36));
        c.fill_rect(r.x, r.y, r.w, 2, rgb(70, 76, 90));
        let col = |v: i32| if v > 50 { rgb(120, 230, 140) } else if v > 25 { rgb(250, 210, 80) } else { rgb(250, 90, 80) };
        let cell = |c: &mut Canvas, x: i32, label: &str, value: String, color: u32| {
            c.text(x, r.y + 8, label, Size::Small, false, rgb(150, 156, 170));
            c.text(x, r.y + 26, &value, Size::Large, true, color);
        };
        cell(c, r.x + 20, "Health", format!("{}", self.kit.health), col(self.kit.health));
        cell(c, r.x + 140, "Armour", format!("{}", self.kit.armour), rgb(120, 170, 255));
        let ammo = match self.kit.weapon {
            1 => format!("{}", self.kit.shells),
            2 => format!("{}", self.kit.cells),
            _ => String::from("--"),
        };
        cell(c, r.x + 260, WEAPONS[self.kit.weapon], ammo, rgb(250, 210, 120));
        // Weapons owned.
        for k in 0..3 {
            let (x, y) = (r.x + 420 + k as i32 * 30, r.y + 18);
            let have = self.kit.have[k];
            c.round_rect(x, y, 24, 28, 4, if self.kit.weapon == k { rgb(250, 210, 120) } else if have { rgb(80, 86, 100) } else { rgb(40, 42, 50) });
            c.text_centered(x + 12, y + 6, &format!("{}", k + 1), Size::Small, true, if have { rgb(20, 20, 24) } else { rgb(70, 72, 80) });
        }
        // Keys.
        for (k, colr) in [rgb(220, 40, 40), rgb(40, 110, 240)].iter().enumerate() {
            let x = r.x + 530 + k as i32 * 34;
            if self.keys[k] {
                c.round_rect(x, r.y + 20, 26, 22, 4, *colr);
            } else {
                c.outline_rect(x, r.y + 20, 26, 22, rgb(70, 72, 80));
            }
        }
        let info = format!("{}   Robots {}/{}", LEVELS[self.level].0, self.kills.0, self.kills.1);
        let iw = crate::gfx::text_width(&info, Size::Small, true);
        c.text(r.x + r.w - iw - 20, r.y + 14, &info, Size::Small, true, rgb(200, 204, 214));
        c.text(r.x + r.w - 300, r.y + 36, "Tab: map   Esc: pause", Size::Small, false, rgb(120, 126, 140));
    }

    fn draw_map(&self, c: &mut Canvas, r: Rect) {
        let s = ((r.h - 40) / self.mh as i32).min((r.w - 40) / self.mw as i32).max(4);
        let (ox, oy) = (r.x + (r.w - s * self.mw as i32) / 2, r.y + (r.h - s * self.mh as i32) / 2);
        c.fill_rect_alpha(r.x, r.y, r.w, r.h, 0, 170);
        for y in 0..self.mh {
            for x in 0..self.mw {
                if !self.seen[y * self.mw + x] {
                    continue;
                }
                let cell = self.map[y * self.mw + x];
                let col = match cell {
                    EMPTY => continue,
                    DOOR => rgb(240, 190, 40),
                    DOOR_RED => rgb(220, 40, 40),
                    DOOR_BLUE => rgb(40, 110, 240),
                    EXIT => rgb(80, 240, 110),
                    _ => rgb(120, 130, 150),
                };
                c.fill_rect(ox + x as i32 * s, oy + y as i32 * s, s - 1, s - 1, col);
            }
        }
        let (px, py) = (ox + (self.x * s as f32) as i32, oy + (self.y * s as f32) as i32);
        c.circle(px, py, (s / 3).max(2), rgb(255, 255, 255));
        super::line(c, px, py, px + (libm::cosf(self.angle) * s as f32) as i32, py + (libm::sinf(self.angle) * s as f32) as i32, 2, rgb(255, 255, 255));
    }

    fn title_buttons(r: Rect) -> ([Rect; 3], [Rect; 3]) {
        let y = r.y + r.h / 2 + 10;
        let diff = core::array::from_fn(|k| Rect::new(r.x + r.w / 2 - 210 + k as i32 * 145, y, 130, 40));
        let start = core::array::from_fn(|k| Rect::new(r.x + r.w / 2 - 210 + k as i32 * 145, y + 70, 130, 40));
        (diff, start)
    }

    fn draw_title(&self, c: &mut Canvas, r: Rect) {
        c.gradient_v(r.x, r.y, r.w, r.h, rgb(8, 10, 24), rgb(40, 14, 30));
        let mut seed = 77u32;
        for _ in 0..180 {
            let (x, y) = (r.x + (noise(&mut seed) % r.w.max(1) as u32) as i32, r.y + (noise(&mut seed) % r.h.max(1) as u32) as i32);
            c.blend(x, y, rgb(220, 220, 255), 200);
        }
        c.text_centered(r.x + r.w / 2, r.y + r.h / 2 - 150, "STARBASE BREACH", Size::Large, true, rgb(255, 120, 60));
        c.text_centered(r.x + r.w / 2, r.y + r.h / 2 - 104, "Robots have taken the station. Clear three decks and stop the Warden.", Size::Small, false, rgb(210, 210, 230));
        c.text_centered(r.x + r.w / 2, r.y + r.h / 2 - 30, "How tough?", Size::Small, true, rgb(200, 200, 220));
        let (diff, start) = Self::title_buttons(r);
        for k in 0..3 {
            super::button(c, diff[k], DIFFICULTY[k].0, self.difficulty == k);
            let label = ["Start: Deck 1", "Deck 2", "Deck 3"][k];
            let open = k <= self.best_level;
            if open {
                super::button(c, start[k], label, k == 0);
            } else {
                c.round_rect(start[k].x, start[k].y, start[k].w, start[k].h, 8, rgb(50, 50, 60));
                c.text_centered(start[k].x + start[k].w / 2, start[k].y + 12, "Locked", Size::Small, true, rgb(110, 110, 120));
            }
        }
        let help = [
            "Move: W A S D or the arrow keys     Run: Shift",
            "Fire: Ctrl, F or the mouse button   Open doors / switches: Space or E",
            "Weapons: 1 2 3     Map: Tab     Pause: Esc",
        ];
        for (k, h) in help.iter().enumerate() {
            c.text_centered(r.x + r.w / 2, start[0].y + 70 + k as i32 * 24, h, Size::Small, false, rgb(170, 170, 200));
        }
    }

    // ---- input ---------------------------------------------------------------------------------

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, sfx: &mut Sfx) -> GameAction {
        if self.state == State::Title {
            let (diff, start) = Self::title_buttons(r);
            for k in 0..3 {
                if diff[k].contains(x, y) {
                    self.difficulty = k;
                    return GameAction::Redraw;
                }
                if start[k].contains(x, y) && k <= self.best_level {
                    self.new_game(k);
                    return GameAction::Redraw;
                }
            }
            return GameAction::None;
        }
        if self.state == State::Playing {
            self.held |= K_FIRE;
            self.fire(sfx);
            return GameAction::Redraw;
        }
        GameAction::None
    }

    pub fn on_release(&mut self) -> bool {
        self.held &= !K_FIRE;
        false
    }

    pub fn on_key(&mut self, sc: u8, sfx: &mut Sfx) -> Option<GameAction> {
        let down = sc & 0x80 == 0;
        let code = sc & 0x7F;
        match self.state {
            State::Title => {
                if down && (code == 0x1C || code == 0x39) {
                    self.new_game(0);
                    return Some(GameAction::Redraw);
                }
                return None;
            }
            State::Dead => {
                if down && code == 0x1C {
                    self.kit = self.kit_at_start.clone();
                    self.kit.health = self.kit.health.max(100);
                    self.load(self.level);
                    self.state = State::Playing;
                }
                return Some(GameAction::Redraw);
            }
            State::LevelDone => {
                if down && code == 0x1C {
                    self.load(self.level + 1);
                    self.state = State::Playing;
                }
                return Some(GameAction::Redraw);
            }
            State::Won => {
                if down && code == 0x1C {
                    self.state = State::Title;
                }
                return Some(GameAction::Redraw);
            }
            State::Paused => {
                if down && code == 0x01 {
                    self.state = State::Playing;
                }
                return Some(GameAction::Redraw);
            }
            State::Playing => {}
        }
        let bit = match code {
            0x11 | 0x48 => K_FWD,
            0x1F | 0x50 => K_BACK,
            0x4B => K_LEFT,
            0x4D => K_RIGHT,
            0x1E => K_SLEFT,
            0x20 => K_SRIGHT,
            0x1D | 0x21 => K_FIRE,
            0x2A | 0x36 => K_RUN,
            _ => 0,
        };
        if bit != 0 {
            if down {
                self.held |= bit;
            } else {
                self.held &= !bit;
            }
            return Some(GameAction::None);
        }
        if !down {
            return Some(GameAction::None);
        }
        match code {
            0x39 | 0x12 => self.use_key(sfx),
            0x02..=0x04 => {
                let k = (code - 0x02) as usize;
                if self.kit.have[k] {
                    self.kit.weapon = k;
                }
            }
            0x0F => self.show_map = !self.show_map,
            0x01 => {
                self.state = State::Paused;
                self.held = 0;
            }
            _ => return None,
        }
        Some(GameAction::Redraw)
    }
}
