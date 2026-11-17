//! The HOSTS part of the sidebar, the window for running a command across
//! hosts, and the broadcast strip over the cards.

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Frame, Id, Key, Layout, Margin,
    Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui,
};
use pitwall_core::proto::TaskStatus;

use super::{section_label, status_mark, Action, AMBER, GREEN, MUTED, RED, SEL, TEXT};
use crate::app::{App, Child};
use crate::i18n::{tr, trf};

fn live(app: &App, id: &str) -> bool {
    app.tasks
        .get(id)
        .is_some_and(|t| !matches!(t.status, TaskStatus::Exited { .. }))
}

pub fn section(ui: &mut Ui, app: &mut App, acts: &mut Vec<Action>) {
    app.refresh_hosts(false);
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        section_label(ui, tr("HOSTS"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if app.hosts.is_empty() {
                return;
            }
            if ui
                .add(egui::Button::new(RichText::new(tr("Run…")).small().color(MUTED)).frame(false))
                .on_hover_text(tr("Run a command on several hosts at once"))
                .clicked()
            {
                acts.push(Action::OpenFleet("all".into()));
            }
            if ui
                .add(
                    egui::Button::new(RichText::new(tr("Connect all")).small().color(MUTED))
                        .frame(false),
                )
                .on_hover_text(tr("Open every host side by side; broadcast input types into all of them"))
                .clicked()
            {
                acts.push(Action::ConnectAll("all".into()));
            }
        });
    });
    if app.hosts.is_empty() {
        ui.label(
            RichText::new(tr(
                "No hosts yet. Add them under hosts in config.toml, or as Host entries in ~/.ssh/config.",
            ))
            .small()
            .color(MUTED),
        );
        return;
    }
    let hosts = app.hosts.clone();
    for h in &hosts {
        let sessions = app.sessions_of(&h.name);
        let up = sessions.iter().any(|s| live(app, s));
        host_row(ui, app, h, up, &sessions, acts);
        if sessions.len() > 1 || sessions.iter().any(|s| !live(app, s)) {
            for id in &sessions {
                session_row(ui, app, id, acts);
            }
        }
    }
}

fn host_row(
    ui: &mut Ui,
    app: &App,
    h: &pitwall_core::config::HostConfig,
    up: bool,
    sessions: &[String],
    acts: &mut Vec<Action>,
) {
    let on = app.cards.iter().any(|c| {
        c.child == Child::Terminal && app.tasks.get(&c.item).and_then(|t| t.host.as_deref()) == Some(&h.name)
    });
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    let p = ui.painter();
    if on {
        p.rect_filled(rect, 6.0, SEL);
    } else if resp.hovered() {
        p.rect_filled(rect, 6.0, Color32::from_white_alpha(8));
    }
    let y = rect.center().y;
    // nf-md-server (U+F048B)
    p.text(
        pos2(rect.left() + 14.0, y),
        Align2::CENTER_CENTER,
        '\u{f048b}',
        crate::theme::icon_font(14.0),
        if up { GREEN } else { MUTED },
    );
    let name = p.text(
        pos2(rect.left() + 28.0, y),
        Align2::LEFT_CENTER,
        &h.name,
        FontId::proportional(13.0),
        TEXT,
    );
    let detail = match (&h.user, h.tags.iter().any(|t| t == "ssh-config")) {
        (_, true) => "~/.ssh/config".to_string(),
        (Some(u), _) => format!("{u}@{}", h.addr()),
        (None, _) if h.addr() != h.name => h.addr().to_string(),
        _ => String::new(),
    };
    if name.right() + 40.0 < rect.right() {
        let room = Rect::from_min_max(pos2(name.right() + 8.0, rect.top()), rect.max);
        ui.painter_at(room).text(
            pos2(room.left(), y),
            Align2::LEFT_CENTER,
            detail,
            FontId::proportional(11.0),
            MUTED,
        );
    }
    let first_live = sessions.iter().find(|s| live(app, s)).cloned();
    let resp = resp.on_hover_text(match &first_live {
        Some(_) => tr("Show its session; right-click for more"),
        None => tr("Connect; right-click for more"),
    });
    if resp.clicked() {
        acts.push(match first_live {
            Some(id) => Action::OpenTerm(id),
            None => Action::Connect(h.name.clone()),
        });
    }
    let name = h.name.clone();
    let addr = h.addr().to_string();
    let tags: Vec<String> = h.tags.iter().filter(|t| *t != "ssh-config").cloned().collect();
    resp.context_menu(|ui| {
        if ui.button(tr("New session")).clicked() {
            acts.push(Action::Connect(name.clone()));
            ui.close();
        }
        if ui.button(tr("Run a command…")).clicked() {
            acts.push(Action::OpenFleet(name.clone()));
            ui.close();
        }
        for t in &tags {
            ui.separator();
            if ui.button(trf!("Connect all tagged {tag}", tag = t)).clicked() {
                acts.push(Action::ConnectAll(format!("tag:{t}")));
                ui.close();
            }
            if ui.button(trf!("Run on all tagged {tag}…", tag = t)).clicked() {
                acts.push(Action::OpenFleet(format!("tag:{t}")));
                ui.close();
            }
        }
        ui.separator();
        if ui.button(tr("Copy address")).clicked() {
            acts.push(Action::Copy(addr.clone()));
            ui.close();
        }
    });
}

fn session_row(ui: &mut Ui, app: &App, id: &str, acts: &mut Vec<Action>) {
    let t = &app.tasks[id];
    let (mark, color) = status_mark(t, app.frame);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, Color32::from_white_alpha(8));
    }
    let y = rect.center().y;
    ui.painter().text(
        pos2(rect.left() + 30.0, y),
        Align2::LEFT_CENTER,
        mark,
        FontId::monospace(12.0),
        color,
    );
    let label = if live(app, id) {
        app.label(t)
    } else {
        trf!("{name} (ended)", name = app.label(t))
    };
    ui.painter().text(
        pos2(rect.left() + 44.0, y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.0),
        MUTED,
    );
    if resp.clicked() {
        acts.push(Action::OpenTerm(id.to_string()));
    }
}

/// The `+` at the end of the tab strip while SSH sessions are showing.
pub fn add_menu(ui: &mut Ui, app: &App, acts: &mut Vec<Action>) {
    if app.hosts.is_empty() {
        ui.label(RichText::new(tr("No hosts configured")).color(MUTED));
        return;
    }
    for h in &app.hosts {
        if ui.button(&h.name).clicked() {
            acts.push(Action::Connect(h.name.clone()));
            ui.close();
        }
    }
}

/// keel's strip over a split: whether typing goes to one terminal or all of
/// them. Shown while broadcasting, or once two SSH sessions share the
/// screen. Returns the space left for the cards.
pub fn broadcast_strip(ui: &mut Ui, app: &App, rect: Rect, acts: &mut Vec<Action>) -> Rect {
    let terms = app.terminal_cards();
    let ssh = terms
        .iter()
        .filter(|id| app.tasks.get(*id).is_some_and(|t| t.host.is_some()))
        .count();
    if !app.broadcast && ssh < 2 {
        return rect;
    }
    let strip = Rect::from_min_size(rect.min, vec2(rect.width(), 30.0));
    let mut c = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("broadcast-strip")
            .max_rect(strip)
            .layout(Layout::left_to_right(Align::Center)),
    );
    if app.broadcast {
        c.painter()
            .rect_filled(strip, 6.0, AMBER.gamma_multiply(0.14));
        c.painter().rect_stroke(
            strip,
            6.0,
            Stroke::new(1.0, AMBER.gamma_multiply(0.6)),
            egui::StrokeKind::Inside,
        );
    }
    c.add_space(10.0);
    if app.broadcast {
        c.label(
            RichText::new(trf!(
                "Broadcasting input to {n} terminals",
                n = terms.len()
            ))
            .strong()
            .color(AMBER),
        );
    } else {
        c.label(RichText::new(tr("Typing goes to the focused terminal")).color(MUTED));
    }
    c.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add_space(6.0);
        let label = if app.broadcast {
            tr("Stop broadcasting")
        } else {
            tr("Broadcast input")
        };
        if ui
            .add(egui::Button::selectable(app.broadcast, RichText::new(label).size(12.5)))
            .on_hover_text(crate::keys::hint("broadcast"))
            .clicked()
        {
            acts.push(Action::ToggleBroadcast);
        }
    });
    Rect::from_min_max(pos2(rect.left(), strip.bottom() + 6.0), rect.max)
}

pub fn fleet_window(ctx: &egui::Context, app: &mut App) {
    if app.fleet.is_none() {
        return;
    }
    let mut open = true;
    let mut run = None;
    let matched: Vec<String> = {
        let f = app.fleet.as_ref().unwrap();
        pitwall_core::hosts::select(&app.hosts, f.selector.trim())
            .into_iter()
            .map(|h| h.name.clone())
            .collect()
    };
    egui::Window::new(tr("Run on hosts"))
        .id(Id::unique("fleet-window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(600.0)
        .default_pos(ctx.content_rect().center_top() + vec2(-300.0, 110.0))
        .show(ctx, |ui| {
            let f = app.fleet.as_mut().unwrap();
            egui::Grid::new("fleet-grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    ui.label(RichText::new(tr("Hosts")).color(MUTED));
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut f.selector)
                                .hint_text("all, web-*, tag:prod")
                                .desired_width(220.0),
                        );
                        let n = matched.len();
                        ui.label(
                            RichText::new(trf!("{n} matched", n = n))
                                .small()
                                .color(if n == 0 { RED } else { MUTED }),
                        )
                        .on_hover_text(matched.join(", "));
                    });
                    ui.end_row();
                    ui.label(RichText::new(tr("Command")).color(MUTED));
                    let r = ui.add(
                        TextEdit::singleline(&mut f.command)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("uptime")
                            .desired_width(f32::INFINITY),
                    );
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        run = Some(false);
                    }
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let ready = !f.running && !f.command.trim().is_empty() && !matched.is_empty();
                if ui
                    .add_enabled(ready, egui::Button::new(tr("Run")))
                    .clicked()
                {
                    run = Some(false);
                }
                if f.running {
                    ui.spinner();
                    ui.label(RichText::new(tr("running…")).color(MUTED));
                }
                ui.label(
                    RichText::new(tr("Changes go through pitwall's policy first"))
                        .small()
                        .color(MUTED),
                );
            });
            if let Some((tier, reason, hosts)) = f.confirm.clone() {
                Frame::new()
                    .fill(AMBER.gamma_multiply(0.12))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new(trf!(
                                "{tier} command: {reason}",
                                tier = tier.label(),
                                reason = reason
                            ))
                            .color(AMBER),
                        );
                        ui.label(
                            RichText::new(trf!("Runs on {hosts}", hosts = hosts.join(", ")))
                                .small()
                                .color(MUTED),
                        );
                        ui.horizontal(|ui| {
                            if ui
                                .button(RichText::new(tr("Run anyway")).color(RED))
                                .clicked()
                            {
                                run = Some(true);
                            }
                            if ui.button(tr("Cancel")).clicked() {
                                f.confirm = None;
                            }
                        });
                    });
            }
            if let Some(e) = &f.error {
                ui.label(RichText::new(e).color(RED));
            }
            if !f.results.is_empty() {
                ui.separator();
                let few = f.results.len() <= 3;
                ScrollArea::vertical()
                    .max_height(380.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for r in &f.results {
                            result(ui, r, few);
                        }
                    });
            }
        });
    if !open {
        app.fleet = None;
    } else if let Some(confirmed) = run {
        app.run_fleet(confirmed);
    }
}

fn result(ui: &mut Ui, r: &pitwall_core::proto::HostResult, open: bool) {
    let ok = r.error.is_none() && r.exit == Some(0);
    let status = match (&r.error, r.exit) {
        (Some(_), _) => tr("failed").to_string(),
        (None, Some(c)) => trf!("exit {code}", code = c),
        (None, None) => tr("killed").to_string(),
    };
    let head = egui::text::LayoutJob::simple_singleline(
        format!("{}  {}  ·  {status}  ·  {} ms", if ok { "✓" } else { "✗" }, r.host, r.ms),
        FontId::proportional(13.0),
        if ok { GREEN } else { RED },
    );
    egui::CollapsingHeader::new(head)
        .id_salt(("fleet-result", &r.host))
        .default_open(open || !ok)
        .show(ui, |ui| {
            let mono = |ui: &mut Ui, s: &str, c: Color32| {
                if !s.trim().is_empty() {
                    ui.add(
                        egui::Label::new(RichText::new(s.trim_end()).monospace().size(12.0).color(c))
                            .selectable(true),
                    );
                }
            };
            if let Some(e) = &r.error {
                mono(ui, e, RED);
            }
            mono(ui, &r.stdout, TEXT);
            mono(ui, &r.stderr, AMBER);
            if r.error.is_none() && r.stdout.trim().is_empty() && r.stderr.trim().is_empty() {
                ui.label(RichText::new(tr("no output")).small().color(MUTED));
            }
        });
}

/// Along the foot of an SSH session that ended, as keel shows: the last
/// screen stays readable above it.
pub fn ended_bar(ui: &mut Ui, app: &App, id: &str, body: Rect, acts: &mut Vec<Action>) {
    let Some(t) = app.tasks.get(id).filter(|t| t.host.is_some()) else {
        return;
    };
    let TaskStatus::Exited { code } = t.status else {
        return;
    };
    let bar = Rect::from_min_max(
        pos2(body.left() + 8.0, body.bottom() - 44.0),
        pos2(body.right() - 8.0, body.bottom() - 8.0),
    );
    ui.painter()
        .rect_filled(bar, 8.0, crate::theme::c::CARD_HOVER);
    ui.painter().rect_stroke(
        bar,
        8.0,
        Stroke::new(1.0, Color32::from_white_alpha(18)),
        egui::StrokeKind::Inside,
    );
    let mut c = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(("ssh-ended", id))
            .max_rect(bar.shrink2(vec2(12.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    let host = t.host.clone().unwrap_or_default();
    let why = match code {
        Some(0) | None => trf!("Session to {host} ended", host = host),
        Some(c) => trf!("Session to {host} ended (exit {code})", host = host, code = c),
    };
    c.label(RichText::new(why).color(TEXT));
    c.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui.button(tr("Close")).clicked() {
            acts.push(Action::CloseTerminal(id.to_string()));
        }
        if ui
            .add(egui::Button::new(RichText::new(tr("Reconnect")).color(crate::theme::c::ON_ACCENT)).fill(super::ACCENT))
            .clicked()
        {
            acts.push(Action::Reconnect(id.to_string()));
        }
    });
}
