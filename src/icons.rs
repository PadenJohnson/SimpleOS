//! Procedurally drawn app icons: a coloured rounded tile with a white glyph.

use crate::desktop::App;
use crate::gfx::{rgb, Canvas};

const WHITE: u32 = rgb(255, 255, 255);

pub fn tile_color(app: App) -> u32 {
    match app {
        App::Photos => rgb(46, 134, 222),
        App::Videos => rgb(231, 76, 60),
        App::Documents => rgb(39, 174, 96),
        App::Printers => rgb(142, 68, 173),
        App::Import => rgb(243, 156, 18),
        App::Settings => rgb(96, 110, 128),
    }
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
