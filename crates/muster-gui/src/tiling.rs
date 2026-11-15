//! Cards on the stage: the grid they sit in and the Mod-key bindings that move
//! focus and cards around. From keel's tiling.rs, without its stacked panes.

use egui::{pos2, vec2, Event, Key, Modifiers, Rect};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TileMod {
    #[default]
    Alt,
    CtrlAlt,
    Off,
}

impl TileMod {
    pub const ALL: [TileMod; 3] = [TileMod::Alt, TileMod::CtrlAlt, TileMod::Off];

    pub fn label(self) -> &'static str {
        match (self, cfg!(target_os = "macos")) {
            // The UI font has no ⌥ / ⌃ glyphs; only the monospace one does.
            (TileMod::Alt, true) => "Option",
            (TileMod::Alt, false) => "Alt",
            (TileMod::CtrlAlt, true) => "Control+Option",
            (TileMod::CtrlAlt, false) => "Ctrl+Alt",
            (TileMod::Off, _) => "Off",
        }
    }

    /// Some(shift) when the modifiers are exactly Mod or Mod+Shift.
    fn matches(self, m: Modifiers) -> Option<bool> {
        let ctrl = match self {
            TileMod::Alt => false,
            TileMod::CtrlAlt => true,
            TileMod::Off => return None,
        };
        (m.alt && m.ctrl == ctrl && !m.mac_cmd).then_some(m.shift)
    }
}

/// How a key chord reads in this OS's conventions, e.g. "⌥⇧H" or "Alt+Shift+H".
pub fn chord(m: TileMod, shift: bool, key: &str) -> String {
    if cfg!(target_os = "macos") {
        let pre = if m == TileMod::CtrlAlt {
            "⌃⌥"
        } else {
            "⌥"
        };
        format!("{pre}{}{key}", if shift { "⇧" } else { "" })
    } else {
        let pre = if m == TileMod::CtrlAlt {
            "Ctrl+Alt"
        } else {
            "Alt"
        };
        format!("{pre}+{}{key}", if shift { "Shift+" } else { "" })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileCmd {
    Focus(Dir),
    Swap(Dir),
    NewPane,
    ClosePane,
    Maximize,
}

/// (shift, key, what it does), for the Settings page.
pub const HELP: &[(bool, &str, &str)] = &[
    (
        false,
        "H / J / K / L",
        "Focus the card left / below / above / right",
    ),
    (
        true,
        "H / J / K / L",
        "Swap with the card left / below / above / right",
    ),
    (false, "Return", "New terminal in a card of its own"),
    (false, "W", "Close card"),
    (false, "Z", "Maximize / restore card"),
];

fn bind(key: Key, shift: bool) -> Option<TileCmd> {
    let dir = match key {
        Key::H => Some(Dir::Left),
        Key::J => Some(Dir::Down),
        Key::K => Some(Dir::Up),
        Key::L => Some(Dir::Right),
        _ => None,
    };
    match (dir, key, shift) {
        (Some(d), _, false) => Some(TileCmd::Focus(d)),
        (Some(d), _, true) => Some(TileCmd::Swap(d)),
        (None, Key::Enter, false) => Some(TileCmd::NewPane),
        (None, Key::W, false) => Some(TileCmd::ClosePane),
        (None, Key::Z, false) => Some(TileCmd::Maximize),
        _ => None,
    }
}

/// Pulls this frame's Mod chords out of the input before the terminal sees them.
///
/// The Text event that follows a chord goes too: Alt+H types "˙" on macOS and
/// "h" elsewhere. The physical key is the fallback for that macOS case, where
/// the logical key is the composed character.
pub fn take_cmds(ctx: &egui::Context, m: TileMod) -> Vec<TileCmd> {
    if m == TileMod::Off {
        return vec![];
    }
    ctx.input_mut(|i| {
        let mut out = vec![];
        let mut k = 0;
        while k < i.events.len() {
            if let Event::Key {
                key,
                physical_key,
                pressed: true,
                repeat,
                modifiers,
            } = &i.events[k]
            {
                if let Some(shift) = m.matches(*modifiers) {
                    if let Some(cmd) =
                        bind(*key, shift).or_else(|| physical_key.and_then(|p| bind(p, shift)))
                    {
                        // Holding Mod+W shouldn't close card after card.
                        if !*repeat || matches!(cmd, TileCmd::Focus(_) | TileCmd::Swap(_)) {
                            out.push(cmd);
                        }
                        i.events.remove(k);
                        if matches!(i.events.get(k), Some(Event::Text(_))) {
                            i.events.remove(k);
                        }
                        continue;
                    }
                }
            }
            k += 1;
        }
        out
    })
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
