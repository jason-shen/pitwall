//! Bits of terminal drawing shared by the terminal view and the editor.

use egui::{pos2, vec2, Color32, FontId, Rect, Stroke, Ui, Vec2};

/// Background for views that aren't terminals (editor, diff): their syntax
/// colours are tuned for it, so they don't follow the terminal scheme.
pub const BG: Color32 = Color32::from_rgb(0x12, 0x14, 0x18);

/// Box-drawing and block characters are drawn as geometry so lines join seamlessly
/// regardless of which font supplies the glyph. Returns false if `ch` isn't handled.
pub fn draw_box(painter: &egui::Painter, ch: char, r: Rect, color: Color32) -> bool {
    // (up, down, left, right) weights: 0 none, 1 light, 2 heavy
    let (u, d, l, rt): (u8, u8, u8, u8) = match ch {
        '─' | '╌' | '┄' | '┈' => (0, 0, 1, 1),
        '━' | '╍' | '┅' | '┉' => (0, 0, 2, 2),
        '│' | '╎' | '┆' | '┊' => (1, 1, 0, 0),
        '┃' | '╏' | '┇' | '┋' => (2, 2, 0, 0),
        '┌' | '╭' => (0, 1, 0, 1),
        '┐' | '╮' => (0, 1, 1, 0),
        '└' | '╰' => (1, 0, 0, 1),
        '┘' | '╯' => (1, 0, 1, 0),
        '├' => (1, 1, 0, 1),
        '┤' => (1, 1, 1, 0),
        '┬' => (0, 1, 1, 1),
        '┴' => (1, 0, 1, 1),
        '┼' => (1, 1, 1, 1),
        '┏' => (0, 2, 0, 2),
        '┓' => (0, 2, 2, 0),
        '┗' => (2, 0, 0, 2),
        '┛' => (2, 0, 2, 0),
        '═' => (0, 0, 2, 2),
        '║' => (2, 2, 0, 0),
        '╔' => (0, 2, 0, 2),
        '╗' => (0, 2, 2, 0),
        '╚' => (2, 0, 0, 2),
        '╝' => (2, 0, 2, 0),
        '┣' => (2, 2, 0, 2),
        '┫' => (2, 2, 2, 0),
        '┳' => (0, 2, 2, 2),
        '┻' => (2, 0, 2, 2),
        '╋' => (2, 2, 2, 2),
        '╠' => (2, 2, 0, 2),
        '╣' => (2, 2, 2, 0),
        '╦' => (0, 2, 2, 2),
        '╩' => (2, 0, 2, 2),
        '╬' => (2, 2, 2, 2),
        '╴' => (0, 0, 1, 0),
        '╶' => (0, 0, 0, 1),
        '╵' => (1, 0, 0, 0),
        '╷' => (0, 1, 0, 0),
        '⏺' | '●' | '⬤' => {
            painter.circle_filled(r.center(), r.width().min(r.height()) * 0.36, color);
            return true;
        }
        '█' => {
            painter.rect_filled(r, 0.0, color);
            return true;
        }
        '▀' => {
            painter.rect_filled(
                Rect::from_min_max(r.min, pos2(r.max.x, r.center().y)),
                0.0,
                color,
            );
            return true;
        }
        '▄' => {
            painter.rect_filled(
                Rect::from_min_max(pos2(r.min.x, r.center().y), r.max),
                0.0,
                color,
            );
            return true;
        }
        '▌' => {
            painter.rect_filled(
                Rect::from_min_max(r.min, pos2(r.center().x, r.max.y)),
                0.0,
                color,
            );
            return true;
        }
        '▐' => {
            painter.rect_filled(
                Rect::from_min_max(pos2(r.center().x, r.min.y), r.max),
                0.0,
                color,
            );
            return true;
        }
        '░' | '▒' | '▓' => {
            shade(painter, ch, r, color);
            return true;
        }
        _ => return false,
    };
    let c = r.center();
    let w = |weight: u8| if weight >= 2 { 2.0 } else { 1.0 };
    let seg = |a: egui::Pos2, b: egui::Pos2, weight: u8| {
        if weight > 0 {
            painter.line_segment([a, b], Stroke::new(w(weight), color));
        }
    };
    seg(c, pos2(c.x, r.min.y), u);
    seg(c, pos2(c.x, r.max.y), d);
    seg(pos2(r.min.x, c.y), c, l);
    seg(c, pos2(r.max.x, c.y), rt);
    true
}

/// ░ ▒ ▓ as a dot pattern on the screen's own pixels, as keel draws them,
/// rather than a flat tint that reads as a dimmer block. The pattern is
/// laid on the whole window's pixel grid, so a run of cells tiles without
/// seams; each cell covers the pixels from its left/top edge up to, not
/// including, its right/bottom one.
fn shade(painter: &egui::Painter, ch: char, r: Rect, color: Color32) {
    let ppp = painter.ctx().pixels_per_point();
    let px = 1.0 / ppp;
    let (x0, x1) = ((r.min.x * ppp).ceil() as i64, (r.max.x * ppp).ceil() as i64);
    let (y0, y1) = ((r.min.y * ppp).ceil() as i64, (r.max.y * ppp).ceil() as i64);
    let lit = |x: i64, y: i64| {
        let (odd_x, odd_y) = (x.rem_euclid(2) == 1, y.rem_euclid(2) == 1);
        match ch {
            '░' => !odd_x && !odd_y,
            '▒' => odd_x == odd_y,
            _ => !(odd_x && odd_y),
        }
    };
    let mut mesh = egui::Mesh::default();
    for y in y0..y1 {
        // Runs of lit pixels, so a full row of ▓ is one quad, not dozens.
        let mut x = x0;
        while x < x1 {
            if !lit(x, y) {
                x += 1;
                continue;
            }
            let start = x;
            while x < x1 && lit(x, y) {
                x += 1;
            }
            mesh.add_colored_rect(
                Rect::from_min_max(
                    pos2(start as f32 * px, y as f32 * px),
                    pos2(x as f32 * px, (y + 1) as f32 * px),
                ),
                color,
            );
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

pub fn cell_size(ui: &Ui, font: &FontId, line_height: f32) -> Vec2 {
    ui.fonts_mut(|f| {
        vec2(
            f.glyph_width(font, 'M'),
            (f.row_height(font) * line_height).round(),
        )
    })
}
