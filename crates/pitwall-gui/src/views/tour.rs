//! The first run: pick a language, then an optional walk around the window
//! that lights up one part at a time. Views mark where each part was drawn
//! this frame with `mark`; the tour reads those back after everything else.

use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Id, Key, Margin, Rect, RichText, Sense, Stroke};

use super::{ACCENT, FAINT, MUTED, TEXT};
use crate::app::App;
use crate::i18n::{tr, trf, Lang};
use crate::settings::SidebarTab;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Onboard {
    Language,
    Tour(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Spot {
    Search,
    Tabs,
    Projects,
    Agents,
    Strip,
    Stage,
    Footer,
}

/// Remember where `spot` was drawn. Only read back in the same pass, so a
/// panel that's gone (sidebar hidden) doesn't leave a stale hole.
pub fn mark(ctx: &egui::Context, spot: Spot, rect: Rect) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(Id::unique(("tour-spot", spot)), (rect, pass)));
}

fn spot_rect(ctx: &egui::Context, spot: Spot) -> Option<Rect> {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<(Rect, u64)>(Id::unique(("tour-spot", spot))))
        .filter(|(r, p)| *p == pass && r.is_positive())
        .map(|(r, _)| r)
}

fn steps() -> [(Spot, &'static str, String); 7] {
    [
        (
            Spot::Search,
            tr("Search or start anything"),
            trf!(
                "Find a project, terminal or file, or type a task and press Enter to start an agent on it. {key} opens this from anywhere.",
                key = crate::keys::hint("command_bar")
            ),
        ),
        (
            Spot::Tabs,
            tr("Projects, Remotes and Keys"),
            tr("Projects lists your folders and what runs in each. Remotes keeps your SSH servers, in groups. Keys holds API keys, encrypted behind a password.").to_string(),
        ),
        (
            Spot::Projects,
            tr("Your projects"),
            tr("Each project folds open to its agents and terminals. Click one to bring it up; + opens another folder.").to_string(),
        ),
        (
            Spot::Agents,
            tr("Agents"),
            tr("Every agent in every project and remote, with what it's doing. One turns amber when it needs an answer or wants to run a command.").to_string(),
        ),
        (
            Spot::Strip,
            tr("Tabs"),
            tr("The open project's terminals and agents, with Files and Changes at the end. Drag a tab onto the stage to tile it beside another.").to_string(),
        ),
        (
            Spot::Stage,
            tr("The stage"),
            tr("Terminals and agents tile here. Drag a card by its title to swap it; drag the gap between cards to resize, double-click it to even them out.").to_string(),
        ),
        (
            Spot::Footer,
            tr("Settings and shortcuts"),
            trf!(
                "Language, fonts, colours and the editor are in Settings ({settings}); Shortcuts ({shortcuts}) lists every key. The green dot is the daemon, which keeps agents running after you close the window.",
                settings = crate::keys::hint("settings"),
                shortcuts = crate::keys::hint("shortcuts")
            ),
        ),
    ]
}

pub fn draw(ctx: &egui::Context, app: &mut App) {
    match app.onboard {
        Some(Onboard::Language) => language(ctx, app),
        Some(Onboard::Tour(i)) => tour(ctx, app, i),
        None => {}
    }
}

fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(14.0).color(theme::c::ON_ACCENT))
        .fill(ACCENT)
        .min_size(vec2(0.0, 30.0))
}

fn language(ctx: &egui::Context, app: &mut App) {
    let mut next = None;
    egui::Modal::new(Id::unique("onboard-language"))
        .backdrop_color(Color32::from_black_alpha(170))
        .show(ctx, |ui| {
            ui.set_width(360.0);
            ui.add_space(4.0);
            ui.label(RichText::new(tr("Welcome to Pitwall")).size(20.0).color(TEXT));
            ui.add_space(2.0);
            ui.label(RichText::new(tr("Pick a language. You can change it later in Settings.")).color(MUTED));
            ui.add_space(12.0);
            let current = Lang::from_code(&app.settings.language);
            for lang in Lang::ALL {
                let on = lang == current;
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
                let p = ui.painter();
                if on {
                    p.rect_filled(r, 6.0, ACCENT.gamma_multiply(0.18));
                    p.rect_stroke(r, 6.0, Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
                } else if resp.hovered() {
                    p.rect_filled(r, 6.0, Color32::from_white_alpha(8));
                }
                p.text(
                    pos2(r.left() + 12.0, r.center().y),
                    Align2::LEFT_CENTER,
                    lang.native_name(),
                    FontId::proportional(14.5),
                    if on { TEXT } else { MUTED },
                );
                if on {
                    p.text(
                        pos2(r.right() - 12.0, r.center().y),
                        Align2::RIGHT_CENTER,
                        "✓",
                        FontId::proportional(14.0),
                        ACCENT,
                    );
                }
                if resp.clicked() {
                    // Applied next frame by apply_settings, so this dialog
                    // redraws in the language you just picked.
                    app.settings.language = lang.code().to_string();
                }
            }
            ui.add_space(14.0);
            ui.label(RichText::new(tr("Want a quick look at where things are?")).color(MUTED));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add(primary(tr("Show me around"))).clicked() {
                    next = Some(true);
                }
                if ui.button(RichText::new(tr("Skip")).size(14.0)).clicked() {
                    next = Some(false);
                }
            });
        });
    if ctx.input(|i| i.key_pressed(Key::Enter)) {
        next = Some(true);
    }
    if let Some(tour) = next {
        // Seen once is enough, whether or not you take the tour or finish it.
        app.settings.welcomed = true;
        app.onboard = tour.then(|| {
            app.sidebar = true;
            app.settings.sidebar_tab = SidebarTab::Projects;
            Onboard::Tour(0)
        });
    }
}

fn tour(ctx: &egui::Context, app: &mut App, i: usize) {
    let steps = steps();
    let i = i.min(steps.len() - 1);
    let last = i + 1 == steps.len();
    let (spot, title, body) = &steps[i];
    let screen = ctx.content_rect();
    let hole = spot_rect(ctx, *spot).map(|r| r.expand(4.0).intersect(screen));

    let mut go: Option<isize> = None;
    let (right, left, esc, enter) = ctx.input(|i| {
        (
            i.key_pressed(Key::ArrowRight),
            i.key_pressed(Key::ArrowLeft),
            i.key_pressed(Key::Escape),
            i.key_pressed(Key::Enter),
        )
    });
    if right || enter {
        go = Some(1);
    }
    if left {
        go = Some(-1);
    }

    let card_w = 320.0_f32.min(screen.width() - 32.0);
    egui::Area::new(Id::unique("onboard-tour"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            // Swallows clicks so the app underneath sits still for the tour.
            ui.interact(screen, Id::unique("onboard-tour-block"), Sense::click_and_drag());
            let p = ui.painter();
            let dim = Color32::from_black_alpha(150);
            match hole {
                Some(h) => {
                    for r in [
                        Rect::from_min_max(screen.min, pos2(screen.right(), h.top())),
                        Rect::from_min_max(pos2(screen.left(), h.bottom()), screen.max),
                        Rect::from_min_max(pos2(screen.left(), h.top()), pos2(h.left(), h.bottom())),
                        Rect::from_min_max(pos2(h.right(), h.top()), pos2(screen.right(), h.bottom())),
                    ] {
                        p.rect_filled(r, 0.0, dim);
                    }
                    p.rect_stroke(h, 8.0, Stroke::new(2.0, ACCENT), egui::StrokeKind::Outside);
                }
                None => {
                    p.rect_filled(screen, 0.0, dim);
                }
            }

            let card = egui::Frame::new()
                .fill(theme::c::PANEL)
                .stroke(Stroke::new(1.0, theme::c::BORDER))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::same(16));
            // Measure once off screen, then place it beside the hole.
            let size = ui
                .ctx()
                .data(|d| d.get_temp::<egui::Vec2>(Id::unique("onboard-card-size")))
                .unwrap_or(vec2(card_w + 32.0, 180.0));
            let pos = place(screen, hole, size);
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_size(pos, vec2(card_w + 32.0, screen.height())))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            let shown = card.show(&mut child, |ui| {
                ui.set_width(card_w);
                ui.label(
                    RichText::new(trf!("{n} of {m}", n = i + 1, m = steps.len()))
                        .small()
                        .color(FAINT),
                );
                ui.add_space(2.0);
                ui.label(RichText::new(*title).size(16.0).color(TEXT));
                ui.add_space(4.0);
                ui.label(RichText::new(body.as_str()).color(MUTED));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if !last
                        && ui
                            .add(egui::Button::new(RichText::new(tr("Skip tour")).color(MUTED)).frame(false))
                            .clicked()
                    {
                        go = Some(isize::MAX);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let next = if last { tr("Done") } else { tr("Next") };
                        if ui.add(primary(next)).clicked() {
                            go = Some(1);
                        }
                        if i > 0 && ui.button(RichText::new(tr("Back")).size(14.0)).clicked() {
                            go = Some(-1);
                        }
                    });
                });
            });
            let measured = shown.response.rect.size();
            if (measured - size).length() > 0.5 {
                ui.ctx().data_mut(|d| d.insert_temp(Id::unique("onboard-card-size"), measured));
                ui.ctx().request_repaint();
            }
        });

    if esc {
        go = Some(isize::MAX);
    }
    if let Some(d) = go {
        let to = i as isize + d;
        app.onboard = if to < 0 {
            Some(Onboard::Tour(0))
        } else if to as usize >= steps.len() {
            None
        } else {
            Some(Onboard::Tour(to as usize))
        };
    }
}

/// Top-left corner for a card of `size` next to `hole`: right of it if it
/// fits, else left, below, above, and the middle of the window when there's
/// no hole or no room.
fn place(screen: Rect, hole: Option<Rect>, size: egui::Vec2) -> egui::Pos2 {
    let pad = 14.0;
    let room = screen.shrink(16.0);
    let fits = |r: Rect| room.contains_rect(r);
    let Some(h) = hole else {
        return screen.center() - size / 2.0;
    };
    let clamp_y = |y: f32| y.clamp(room.top(), (room.bottom() - size.y).max(room.top()));
    let clamp_x = |x: f32| x.clamp(room.left(), (room.right() - size.x).max(room.left()));
    let tries = [
        pos2(h.right() + pad, clamp_y(h.top())),
        pos2(h.left() - pad - size.x, clamp_y(h.top())),
        pos2(clamp_x(h.left()), h.bottom() + pad),
        pos2(clamp_x(h.left()), h.top() - pad - size.y),
    ];
    tries
        .into_iter()
        .find(|&at| fits(Rect::from_min_size(at, size)))
        // The hole fills the window (the stage, on a small screen): sit on
        // top of it, near the bottom.
        .unwrap_or_else(|| pos2(clamp_x(h.center().x - size.x / 2.0), clamp_y(h.bottom() - size.y - 24.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_goes_beside_the_hole() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1400.0, 900.0));
        let size = vec2(350.0, 180.0);
        // Sidebar on the left: the card sits to its right.
        let side = Rect::from_min_max(pos2(0.0, 40.0), pos2(280.0, 300.0));
        let at = place(screen, Some(side), size);
        assert!(at.x >= side.right());
        // Stage takes nearly everything: the card still lands on screen.
        let stage = Rect::from_min_max(pos2(290.0, 40.0), pos2(1400.0, 900.0));
        let at = place(screen, Some(stage), size);
        assert!(screen.contains_rect(Rect::from_min_size(at, size)));
    }
}
