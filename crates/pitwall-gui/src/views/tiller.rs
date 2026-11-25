//! The TILLER part of the sidebar: machines running tillerd and their tmux
//! sessions. Shown when Settings → Tiller hosts is on.

use egui::{Align, Key, Layout, RichText, TextEdit, Ui};
use pitwall_core::proto::TaskStatus;

use super::{Action, AMBER, GREEN, MUTED, RED, TEXT};
use crate::app::App;
use crate::i18n::{tr, trf};

/// The `+` on the sidebar's tab row while Tiller is showing.
pub fn add_button(ui: &mut Ui, acts: &mut Vec<Action>) {
    let plus = ui
        .add(egui::Button::new(RichText::new("+").color(MUTED)).frame(false))
        .on_hover_text(tr("Add a Tiller host"));
    egui::Popup::menu(&plus).show(|ui| {
        if ui.button(tr("Paste a pairing code…")).clicked() {
            acts.push(Action::TillerForm);
            ui.close();
        }
        if ui.button(tr("This machine")).clicked() {
            acts.push(Action::TillerAddLocal);
            ui.close();
        }
    });
}

pub fn section(ui: &mut Ui, app: &mut App, acts: &mut Vec<Action>) {
    if app.tiller.form.is_some() {
        form(ui, app, acts);
        ui.add_space(10.0);
    }
    if app.settings.tiller_hosts.is_empty() {
        if app.tiller.form.is_none() {
            ui.label(
                RichText::new(tr("No Tiller hosts yet. Run tillerd on a machine, open Pair a phone in its console and copy the pairing JSON, then + → Paste a pairing code."))
                    .small()
                    .color(MUTED),
            );
        }
        return;
    }
    let hosts = app.settings.tiller_hosts.clone();
    for h in &hosts {
        // Fetch a host's sessions the first time it's shown.
        if !app.tiller.hosts.get(&h.name).is_some_and(|v| v.loaded || v.loading) {
            acts.push(Action::TillerRefresh(h.name.clone()));
        }
        let view = app.tiller.hosts.get(&h.name).cloned().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(RichText::new(&h.name).strong().color(TEXT));
            ui.label(RichText::new(h.ssh_target()).small().color(MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let remove = ui
                    .add(egui::Button::new(RichText::new("✕").small().color(MUTED)).frame(false))
                    .on_hover_text(tr("Remove this host"));
                if remove.clicked() {
                    acts.push(Action::TillerRemove(h.name.clone()));
                }
                let refresh = ui
                    .add_enabled(!view.loading, egui::Button::new(RichText::new("⟳").small().color(MUTED)).frame(false))
                    .on_hover_text(tr("Refresh"));
                if refresh.clicked() {
                    acts.push(Action::TillerRefresh(h.name.clone()));
                }
            });
        });
        if view.loading && !view.loaded {
            ui.label(RichText::new(tr("Connecting…")).small().color(MUTED));
        }
        if let Some(e) = &view.error {
            ui.label(RichText::new(e).small().color(RED));
        }
        for s in &view.sessions {
            let open = app.tasks.values().any(|t| {
                crate::tiller::is_tiller_dir(&t.cwd)
                    && t.name == format!("{} · {}", s.name, h.name)
                    && !matches!(t.status, TaskStatus::Exited { .. })
            });
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                let dot = if open { GREEN } else { MUTED };
                ui.label(RichText::new("●").small().color(dot));
                let label = ui
                    .add(egui::Label::new(RichText::new(&s.name).color(TEXT)).sense(egui::Sense::click()))
                    .on_hover_text(trf!(
                        "{n} window(s) · {path}",
                        n = s.windows,
                        path = if s.path.is_empty() { "~" } else { s.path.as_str() }
                    ));
                if label.clicked() {
                    acts.push(Action::TillerOpen(h.name.clone(), s.name.clone()));
                }
                if s.attached > 0 {
                    ui.label(RichText::new(tr("attached")).small().color(AMBER));
                }
            });
        }
        if view.loaded && view.error.is_none() {
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                let name = app.tiller.new_session.entry(h.name.clone()).or_default();
                let edit = ui.add(
                    TextEdit::singleline(name)
                        .hint_text(tr("new session"))
                        .desired_width(ui.available_width() - 30.0),
                );
                let go = ui.add_enabled(!name.trim().is_empty(), egui::Button::new("+").small());
                if go.clicked() || (edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !name.trim().is_empty()) {
                    acts.push(Action::TillerNewSession(h.name.clone()));
                }
            });
        }
        ui.add_space(10.0);
    }
}

fn form(ui: &mut Ui, app: &mut App, acts: &mut Vec<Action>) {
    ui.label(RichText::new(tr("Pairing code")).strong().color(TEXT));
    ui.label(
        RichText::new(tr("In the tillerd console: Pair a phone → Copy pairing JSON. pitwall connects over SSH with your keys, so the code needs an SSH address."))
            .small()
            .color(MUTED),
    );
    let text = app.tiller.form.get_or_insert_with(String::new);
    ui.add(
        TextEdit::multiline(text)
            .hint_text("{\"v\":1,…}")
            .desired_rows(4)
            .desired_width(f32::INFINITY)
            .code_editor(),
    );
    ui.horizontal(|ui| {
        if ui.button(tr("Add")).clicked() {
            acts.push(Action::TillerAdd);
        }
        if ui.button(tr("Cancel")).clicked() {
            app.tiller.form = None;
        }
    });
}
