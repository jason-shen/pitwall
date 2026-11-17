//! The grid cards sit in on the stage, from keel's tiling.rs, without its
//! stacked panes. The keys that move cards around are in keys.rs.

use egui::{pos2, vec2, Rect};
use serde::{Deserialize, Serialize};

/// The old "card keys use" setting. Only read now, to give an upgraded
/// config the same card keys in [keys].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TileMod {
    #[default]
    Alt,
    CtrlAlt,
    Off,
}

/// Rows and columns of the grid for n panes.
pub fn shape(area: Rect, n: usize) -> (usize, usize) {
    let side = (n as f32).sqrt().ceil() as usize;
    // a tall window stacks rows before adding columns
    let cols = if area.height() > area.width() {
        n.div_ceil(side)
    } else {
        side
    };
    (n.div_ceil(cols), cols)
}

/// Grid for n panes, columns and rows sized by relative weights (missing
/// ones count as 1). A short last row stretches across, using the first
/// columns' weights. `gap.x` separates columns, `gap.y` rows.
pub fn grid_sized(
    area: Rect,
    n: usize,
    gap: egui::Vec2,
    col_w: &[f32],
    row_w: &[f32],
) -> Vec<Rect> {
    if n == 0 {
        return vec![];
    }
    let (rows, cols) = shape(area, n);
    let w_of = |v: &[f32], i: usize| v.get(i).copied().unwrap_or(1.0).max(0.05);
    let row_sum: f32 = (0..rows).map(|r| w_of(row_w, r)).sum();
    let free_h = area.height() - gap.y * (rows as f32 - 1.0);
    let mut out = Vec::with_capacity(n);
    let mut y = area.top();
    for r in 0..rows {
        let count = if r + 1 == rows {
            n - cols * (rows - 1)
        } else {
            cols
        };
        let h = free_h * w_of(row_w, r) / row_sum;
        let col_sum: f32 = (0..count).map(|k| w_of(col_w, k)).sum();
        let free_w = area.width() - gap.x * (count as f32 - 1.0);
        let mut x = area.left();
        for k in 0..count {
            let w = free_w * w_of(col_w, k) / col_sum;
            out.push(Rect::from_min_size(pos2(x, y), vec2(w, h)));
            x += w + gap.x;
        }
        y += h + gap.y;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_size_columns_and_rows() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(1008.0, 600.0));
        // Two side by side, the first three times the second.
        let r = grid_sized(area, 2, vec2(8.0, 8.0), &[3.0, 1.0], &[]);
        assert_eq!(r[0].width(), 750.0);
        assert_eq!(r[1].width(), 250.0);
        assert_eq!(r[1].left(), 758.0);
        // Four in a 2x2, the top row twice the bottom one.
        let r = grid_sized(area, 4, vec2(8.0, 8.0), &[], &[2.0, 1.0]);
        assert!((r[0].height() - 394.666).abs() < 0.01);
        assert_eq!(r[2].top(), r[0].bottom() + 8.0);
        // No weights: even, as before.
        let r = grid_sized(area, 3, vec2(8.0, 8.0), &[], &[]);
        assert_eq!(r[0].width(), r[1].width());
        assert_eq!(r[2].width(), 1008.0);
    }
}
