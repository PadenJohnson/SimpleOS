//! Procedurally drawn app icons: a coloured rounded tile with a white glyph.

use crate::desktop::App;
use crate::gfx::{rgb, Canvas};

const WHITE: u32 = rgb(255, 255, 255);

pub fn tile_color(app: App) -> u32 {
    match app {
        App::Photos => rgb(46, 134, 222),
        App::Videos => rgb(231, 76, 60),
        App::Movie => rgb(211, 84, 0),
        App::Documents => rgb(39, 174, 96),
        App::Printers => rgb(142, 68, 173),
        App::Import => rgb(243, 156, 18),
        App::Files => rgb(22, 160, 133),
        App::Disks => rgb(52, 73, 94),
        App::Settings => rgb(96, 110, 128),
        App::Mines => rgb(60, 90, 140),
        App::Solitaire => rgb(30, 130, 80),
        App::FreeCell => rgb(20, 110, 120),
        App::Sudoku => rgb(200, 140, 40),
        App::Pinball => rgb(70, 40, 130),
        App::Shooter => rgb(150, 50, 30),
    }
}

/// The Games folder in the Applications menu: a joystick.
pub fn games_folder(c: &mut Canvas, x: i32, y: i32) {
    let bg = rgb(230, 80, 140);
    c.round_rect_alpha(x + 2, y + 4, 64, 64, 14, 0, 70);
    c.round_rect(x, y, 64, 64, 14, bg);
    c.round_rect(x + 12, y + 36, 40, 16, 6, WHITE);
    c.fill_rect(x + 30, y + 18, 4, 20, WHITE);
    c.circle(x + 32, y + 18, 8, WHITE);
    c.circle(x + 44, y + 44, 3, bg);
}

/// "Back" in the Games folder.
pub fn back(c: &mut Canvas, x: i32, y: i32) {
    let bg = rgb(150, 158, 170);
    c.round_rect(x, y, 64, 64, 14, bg);
    c.triangle([(x + 16, y + 32), (x + 34, y + 16), (x + 34, y + 48)], WHITE);
    c.fill_rect(x + 32, y + 27, 18, 10, WHITE);
}

/// Draw a 64x64 icon with its top-left at (x, y).
pub fn draw(c: &mut Canvas, app: App, x: i32, y: i32) {
    let bg = tile_color(app);
    c.round_rect_alpha(x + 2, y + 4, 64, 64, 14, 0, 70); // shadow
    c.round_rect(x, y, 64, 64, 14, bg);
    match app {
        App::Photos => {
            c.round_rect(x + 12, y + 16, 40, 32, 4, WHITE);
            c.fill_rect(x + 15, y + 19, 34, 26, bg);
            c.circle(x + 40, y + 26, 4, WHITE);
            c.triangle([(x + 15, y + 44), (x + 27, y + 28), (x + 39, y + 44)], WHITE);
            c.triangle([(x + 31, y + 44), (x + 39, y + 34), (x + 48, y + 44)], WHITE);
        }
        App::Videos => {
            c.circle(x + 32, y + 32, 19, WHITE);
            c.circle(x + 32, y + 32, 16, bg);
            c.triangle([(x + 27, y + 22), (x + 27, y + 42), (x + 43, y + 32)], WHITE);
        }
        App::Mines => {
            // A mine and a flag.
            c.circle(x + 26, y + 36, 12, WHITE);
            for (dx, dy) in [(16, 0), (0, 16), (12, 12), (12, -12)] {
                c.fill_rect(x + 26 - dx / 2 - 1, y + 36 - dy / 2 - 1, 3, 3, WHITE);
                c.fill_rect(x + 26 + dx / 2 - 1, y + 36 + dy / 2 - 1, 3, 3, WHITE);
            }
            c.circle(x + 22, y + 32, 3, bg);
            c.fill_rect(x + 44, y + 14, 3, 26, WHITE);
            c.triangle([(x + 47, y + 14), (x + 47, y + 26), (x + 58, y + 20)], rgb(240, 80, 70));
        }
        App::Solitaire | App::FreeCell => {
            // Two fanned cards.
            c.round_rect(x + 12, y + 14, 26, 36, 4, rgb(220, 226, 236));
            c.round_rect(x + 24, y + 18, 28, 38, 4, WHITE);
            let red = app == App::Solitaire;
            let col = if red { rgb(210, 40, 40) } else { rgb(30, 30, 34) };
            c.circle(x + 34, y + 33, 5, col);
            c.circle(x + 42, y + 33, 5, col);
            c.triangle([(x + 29, y + 35), (x + 47, y + 35), (x + 38, y + 46)], col);
        }
        App::Sudoku => {
            c.round_rect(x + 12, y + 12, 40, 40, 4, WHITE);
            for k in 1..3 {
                c.fill_rect(x + 12 + k * 13, y + 14, 2, 36, bg);
                c.fill_rect(x + 14, y + 12 + k * 13, 36, 2, bg);
            }
            c.text(x + 16, y + 14, "5", crate::gfx::Size::Small, true, bg);
            c.text(x + 42, y + 40, "9", crate::gfx::Size::Small, true, bg);
            c.text(x + 29, y + 27, "3", crate::gfx::Size::Small, true, rgb(40, 90, 200));
        }
        App::Shooter => {
            // A visor with a glowing slit, over a station corridor.
            c.triangle([(x + 8, y + 56), (x + 24, y + 30), (x + 24, y + 56)], rgb(110, 40, 20));
            c.triangle([(x + 56, y + 56), (x + 40, y + 30), (x + 40, y + 56)], rgb(110, 40, 20));
            c.round_rect(x + 14, y + 10, 36, 26, 10, WHITE);
            c.fill_rect(x + 18, y + 19, 28, 6, rgb(255, 150, 60));
            c.fill_rect(x + 24, y + 36, 16, 6, WHITE);
        }
        App::Pinball => {
            // A ball, a flipper and a star.
            c.circle(x + 24, y + 22, 9, WHITE);
            c.circle(x + 21, y + 19, 3, rgb(200, 200, 230));
            c.triangle([(x + 14, y + 46), (x + 50, y + 52), (x + 14, y + 54)], rgb(255, 210, 90));
            c.triangle([(x + 46, y + 14), (x + 42, y + 26), (x + 50, y + 26)], rgb(255, 230, 120));
            c.triangle([(x + 46, y + 30), (x + 38, y + 20), (x + 54, y + 20)], rgb(255, 230, 120));
        }
        App::Movie => {
            // A film strip with a timeline under it.
            c.round_rect(x + 10, y + 12, 44, 28, 4, WHITE);
            for k in 0..5 {
                c.fill_rect(x + 14 + k * 8, y + 15, 4, 3, bg);
                c.fill_rect(x + 14 + k * 8, y + 34, 4, 3, bg);
            }
            c.triangle([(x + 28, y + 20), (x + 28, y + 32), (x + 38, y + 26)], bg);
            c.round_rect(x + 10, y + 45, 20, 7, 2, WHITE);
            c.round_rect(x + 33, y + 45, 21, 7, 2, WHITE);
            c.fill_rect(x + 40, y + 42, 2, 13, rgb(255, 214, 10));
        }
        App::Documents => {
            c.fill_rect(x + 18, y + 12, 22, 40, WHITE);
            c.fill_rect(x + 18, y + 22, 28, 30, WHITE);
            c.triangle([(x + 40, y + 12), (x + 40, y + 22), (x + 46, y + 22)], WHITE);
            for i in 0..4 {
                c.fill_rect(x + 23, y + 27 + i * 6, 18, 2, bg);
            }
        }
        App::Printers => {
            c.fill_rect(x + 22, y + 12, 20, 12, WHITE); // paper in
            c.round_rect(x + 12, y + 24, 40, 18, 4, WHITE); // body
            c.fill_rect(x + 20, y + 36, 24, 2, bg); // slot
            c.fill_rect(x + 22, y + 42, 20, 12, WHITE); // paper out
            c.fill_rect(x + 25, y + 45, 14, 2, bg);
            c.fill_rect(x + 25, y + 49, 10, 2, bg);
            c.circle(x + 46, y + 29, 2, bg);
        }
        App::Settings => {
            // Three sliders.
            for (i, knob) in [(0, 22), (1, 40), (2, 28)] {
                let yy = y + 20 + i * 12;
                c.fill_rect(x + 14, yy, 36, 3, WHITE);
                c.circle(x + knob, yy + 1, 5, WHITE);
                c.circle(x + knob, yy + 1, 3, bg);
            }
        }
        App::Files => {
            // A folder.
            c.round_rect(x + 12, y + 17, 18, 10, 3, WHITE);
            c.round_rect(x + 12, y + 22, 40, 26, 4, WHITE);
            c.fill_rect(x + 12, y + 28, 40, 2, bg);
        }
        App::Disks => {
            // A disk drive with its light, and a pie of used space.
            c.round_rect(x + 12, y + 34, 40, 16, 4, WHITE);
            c.circle(x + 44, y + 42, 2, bg);
            c.fill_rect(x + 17, y + 41, 16, 2, bg);
            c.circle(x + 32, y + 22, 10, WHITE);
            c.triangle([(x + 32, y + 22), (x + 32, y + 12), (x + 42, y + 22)], bg);
        }
        App::Import => {
            c.round_rect(x + 22, y + 22, 20, 32, 4, WHITE); // stick body
            c.fill_rect(x + 25, y + 10, 14, 12, WHITE); // connector
            c.fill_rect(x + 28, y + 13, 3, 4, bg);
            c.fill_rect(x + 33, y + 13, 3, 4, bg);
            c.triangle([(x + 26, y + 34), (x + 38, y + 34), (x + 32, y + 42)], bg); // arrow
            c.fill_rect(x + 30, y + 27, 4, 8, bg);
        }
    }
}
