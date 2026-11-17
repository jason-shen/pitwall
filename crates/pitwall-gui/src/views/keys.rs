//! The Keys tab: API keys kept encrypted behind their own password, and the
//! dialogs for adding one, typing the password and changing it.

use egui::{pos2, vec2, Align, Align2, Color32, FontId, Id, Key, Layout, RichText, Sense, TextEdit, Ui};

use super::{Action, ACCENT, AMBER, MUTED, RED, SEL, TEXT};
use crate::apikeys::{masked, Action as KeyAction, Change};
use crate::app::{App, Confirm};
use crate::i18n::{tr, trf};
use crate::theme;

// nf-md-key, nf-md-content_copy, nf-md-eye, nf-md-eye_off
const KEY: char = '\u{f0306}';
const COPY: char = '\u{f018f}';
const EYE: char = '\u{f0208}';
const EYE_OFF: char = '\u{f0209}';

fn pw_edit<'a>(value: &'a mut String, hint: &'a str) -> TextEdit<'a> {
    TextEdit::singleline(value)
        .password(true)
        .hint_text(hint)
        .desired_width(f32::INFINITY)
}

fn icon_button(ui: &mut Ui, icon: char, tip: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(icon.to_string()).font(theme::icon_font(13.0)).color(MUTED))
            .frame(false),
    )
    .on_hover_text(tip)
}

/// The `+` on the sidebar's tab row while Keys is showing.
pub fn add_button(ui: &mut Ui, app: &mut App, acts: &mut Vec<Action>) {
    if app.api_keys.lock.is_none() {
        return;
    }
    let plus = ui
        .add(egui::Button::new(RichText::new("+").color(MUTED)).frame(false))
        .on_hover_text(tr("Add an API key"));
    egui::Popup::menu(&plus).show(|ui| {
        if ui.button(tr("Add a key…")).clicked() {
            app.keys_open_editor(None);
            ui.close();
        }
        if ui.button(tr("Change password…")).clicked() {
            app.api_ui.change = Some(Change::default());
            ui.close();
        }
        if app.api_ui.unlocked() && ui.button(tr("Lock now")).clicked() {
            app.api_ui.forget();
            ui.close();
        }
        ui.separator();
        if ui
            .button(RichText::new(tr("Forgot the password…")).color(RED))
            .clicked()
        {
            acts.push(Action::ConfirmResetKeys);
            ui.close();
        }
    });
}

pub fn section(ui: &mut Ui, app: &mut App, acts: &mut Vec<Action>) {
    if app.api_keys.lock.is_none() {
        return setup(ui, app);
    }
    if app.api_keys.keys.is_empty() {
        ui.label(
            RichText::new(tr("No API keys yet. + adds one; it's encrypted with your password before it's saved."))
                .small()
                .color(MUTED),
        );
        if ui.button(tr("Add a key")).clicked() {
            app.keys_open_editor(None);
        }
        return;
    }
    let keys = app.api_keys.keys.clone();
    for k in &keys {
        row(ui, app, k, acts);
    }
    ui.add_space(8.0);
    ui.label(
        RichText::new(tr("Revealing or copying a key asks for the password. Copied keys leave the clipboard after 30 seconds."))
            .small()
            .color(crate::theme::c::TEXT_FAINT),
    );
}

/// First visit: pick the password everything here is encrypted with.
fn setup(ui: &mut Ui, app: &mut App) {
    ui.add_space(4.0);
    ui.label(RichText::new(tr("Protect your API keys")).strong().color(TEXT));
    ui.add_space(2.0);
    ui.label(
        RichText::new(tr("Every key is encrypted with this password, and pitwall asks for it whenever you reveal or copy one. A forgotten password can't be recovered."))
            .small()
            .color(MUTED),
    );
    ui.add_space(8.0);
    let k = &mut app.api_ui;
    ui.add(pw_edit(&mut k.setup_pw, tr("Password (8 characters or more)")));
    let r = ui.add(pw_edit(&mut k.setup_confirm, tr("Confirm password")));
    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    if let Some(e) = &k.setup_error {
        ui.label(RichText::new(e).small().color(RED));
    }
    ui.add_space(4.0);
    let busy = k.busy();
    ui.horizontal(|ui| {
        let set = ui.add_enabled(
            !busy,
            egui::Button::new(RichText::new(tr("Set password")).color(theme::c::ON_ACCENT)).fill(ACCENT),
        );
        if busy {
            ui.spinner();
        }
        if (set.clicked() || enter) && !busy {
            app.keys_setup();
        }
    });
}

fn row(ui: &mut Ui, app: &mut App, k: &crate::apikeys::ApiKey, acts: &mut Vec<Action>) {
    let revealed = app
        .api_ui
        .revealed
        .as_ref()
        .filter(|r| r.0 == k.id)
        .map(|r| (r.1.clone(), r.2));
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click());
    let p = ui.painter().clone();
    if revealed.is_some() {
        p.rect_filled(rect, 6.0, SEL);
    } else if resp.hovered() {
        p.rect_filled(rect, 6.0, Color32::from_white_alpha(8));
    }
    let top = rect.top() + 13.0;
    let bottom = rect.bottom() - 12.0;
    p.text(
        pos2(rect.left() + 12.0, top),
        Align2::CENTER_CENTER,
        KEY,
        theme::icon_font(13.0),
        AMBER,
    );
    // Buttons first, so the text knows where to stop.
    let mut buttons = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_max(
                pos2(rect.right() - 60.0, rect.top()),
                pos2(rect.right() - 4.0, rect.top() + 26.0),
            ))
            .layout(Layout::right_to_left(Align::Center)),
    );
    if icon_button(&mut buttons, COPY, tr("Copy (asks for the password)")).clicked() {
        app.keys_request(KeyAction::Copy(k.id.clone()));
    }
    let (eye, tip) = if revealed.is_some() {
        (EYE_OFF, tr("Hide"))
    } else {
        (EYE, tr("Reveal (asks for the password)"))
    };
    if icon_button(&mut buttons, eye, tip).clicked() {
        if revealed.is_some() {
            app.api_ui.revealed = None;
        } else {
            app.keys_request(KeyAction::Reveal(k.id.clone()));
        }
    }
    let text_right = rect.right() - 64.0;
    let clip = p.with_clip_rect(egui::Rect::from_min_max(
        pos2(rect.left() + 24.0, rect.top()),
        pos2(text_right, rect.bottom()),
    ));
    let mut title = egui::text::LayoutJob::default();
    title.append(
        &k.name,
        0.0,
        egui::TextFormat::simple(FontId::proportional(13.0), TEXT),
    );
    if !k.service.is_empty() {
        title.append(
            &format!("  {}", k.service),
            0.0,
            egui::TextFormat::simple(FontId::proportional(11.0), MUTED),
        );
    }
    let g = ui.fonts_mut(|f| f.layout_job(title));
    clip.galley(pos2(rect.left() + 24.0, top - g.size().y / 2.0), g, TEXT);
    let clip = p.with_clip_rect(egui::Rect::from_min_max(
        pos2(rect.left() + 24.0, rect.top()),
        pos2(rect.right() - 6.0, rect.bottom()),
    ));
    match &revealed {
        Some((value, until)) => {
            let left = until.saturating_duration_since(std::time::Instant::now()).as_secs() + 1;
            let g = clip.text(
                pos2(rect.left() + 24.0, bottom),
                Align2::LEFT_CENTER,
                value,
                FontId::monospace(11.5),
                ACCENT,
            );
            if g.right() + 30.0 < rect.right() {
                clip.text(
                    pos2(rect.right() - 8.0, bottom),
                    Align2::RIGHT_CENTER,
                    format!("{left}s"),
                    FontId::proportional(10.5),
                    MUTED,
                );
            }
        }
        None => {
            clip.text(
                pos2(rect.left() + 24.0, bottom),
                Align2::LEFT_CENTER,
                masked(&k.hint),
                FontId::monospace(11.5),
                MUTED,
            );
        }
    }
    let resp = resp.on_hover_text(if k.notes.is_empty() {
        tr("Right-click to edit or delete").to_string()
    } else {
        k.notes.clone()
    });
    let id = k.id.clone();
    let name = k.name.clone();
    resp.context_menu(|ui| {
        if ui.button(tr("Reveal")).clicked() {
            acts.push(Action::KeyRequest(KeyAction::Reveal(id.clone())));
            ui.close();
        }
        if ui.button(tr("Copy")).clicked() {
            acts.push(Action::KeyRequest(KeyAction::Copy(id.clone())));
            ui.close();
        }
        if ui.button(tr("Edit…")).clicked() {
            acts.push(Action::EditKey(id.clone()));
            ui.close();
        }
        ui.separator();
        if ui.button(RichText::new(tr("Delete…")).color(RED)).clicked() {
            acts.push(Action::ConfirmDeleteKey(id.clone(), name.clone()));
            ui.close();
        }
    });
}

pub fn confirm_delete(app: &mut App, id: String, name: &str) {
    app.confirm = Some(Confirm::DeleteApiKey {
        id,
        text: trf!("Delete the API key {name}? It can't be brought back.", name = name),
    });
}

pub fn confirm_reset(app: &mut App) {
    app.confirm = Some(Confirm::ResetApiKeys {
        text: tr("Forgot the password? The keys can't be decrypted without it. Delete every saved API key and set a new password?").into(),
    });
}

/// The editor, the password prompt and the change-password dialog.
pub fn dialogs(ctx: &egui::Context, app: &mut App) {
    editor(ctx, app);
    prompt(ctx, app);
    change(ctx, app);
}

fn modal(ctx: &egui::Context, id: &str, width: f32, body: impl FnOnce(&mut Ui)) {
    let id = Id::unique(id);
    egui::Modal::new(id)
        .area(egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, vec2(0.0, 110.0)))
        .show(ctx, |ui| {
            ui.set_width(width);
            body(ui)
        });
}

fn editor(ctx: &egui::Context, app: &mut App) {
    if app.api_ui.editor.is_none() || app.api_ui.prompt.is_some() {
        return;
    }
    let (enter, esc) = ctx.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
    let (mut save, mut cancel) = (enter, esc);
    modal(ctx, "api-key-editor", 460.0, |ui| {
        let ed = app.api_ui.editor.as_mut().unwrap();
        let new = ed.id.is_none();
        ui.label(
            RichText::new(if new { tr("Add an API key") } else { tr("Edit API key") })
                .size(16.0)
                .strong()
                .color(TEXT),
        );
        ui.add_space(8.0);
        egui::Grid::new("api-key-grid")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label(RichText::new(tr("Name")).color(MUTED));
                let r = ui.add(
                    TextEdit::singleline(&mut ed.name)
                        .hint_text(tr("OpenAI personal"))
                        .desired_width(f32::INFINITY),
                );
                if new && ui.memory(|m| m.focused().is_none()) {
                    r.request_focus();
                }
                ui.end_row();
                ui.label(RichText::new(tr("Service")).color(MUTED));
                ui.add(
                    TextEdit::singleline(&mut ed.service)
                        .hint_text(tr("optional, e.g. openai.com"))
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
                ui.label(RichText::new(if new { tr("Key") } else { tr("Replace key") }).color(MUTED));
                ui.horizontal(|ui| {
                    let eye = if ed.show { EYE_OFF } else { EYE };
                    if icon_button(ui, eye, if ed.show { tr("Hide") } else { tr("Show") }).clicked() {
                        ed.show = !ed.show;
                    }
                    ui.add(
                        TextEdit::singleline(&mut ed.secret)
                            .password(!ed.show)
                            .font(egui::TextStyle::Monospace)
                            .hint_text(if new { "sk-…" } else { tr("leave blank to keep the saved key") })
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.end_row();
                ui.label(RichText::new(tr("Notes")).color(MUTED));
                ui.add(
                    TextEdit::singleline(&mut ed.notes)
                        .hint_text(tr("optional"))
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
            });
        if let Some(e) = &ed.error {
            ui.add_space(4.0);
            ui.label(RichText::new(e).color(RED));
        }
        ui.add_space(10.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(egui::Button::new(RichText::new(tr("Save")).color(theme::c::ON_ACCENT)).fill(ACCENT))
                .clicked()
            {
                save = true;
            }
            if ui.button(tr("Cancel")).clicked() {
                cancel = true;
            }
        });
    });
    if cancel {
        app.api_ui.editor = None;
    } else if save {
        app.keys_save_editor();
    }
}

fn prompt(ctx: &egui::Context, app: &mut App) {
    let Some(action) = app.api_ui.prompt.as_ref().map(|p| p.action.clone()) else {
        return;
    };
    let busy = app.api_ui.busy();
    let (enter, esc) = ctx.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
    let (mut go, mut cancel) = (enter && !busy, esc);
    let (title, verb) = match &action {
        KeyAction::Reveal(_) => (tr("Reveal API key"), tr("Reveal")),
        KeyAction::Copy(_) => (tr("Copy API key"), tr("Copy")),
        KeyAction::Save => (tr("Save API key"), tr("Save")),
    };
    modal(ctx, "api-key-prompt", 380.0, |ui| {
        let p = app.api_ui.prompt.as_mut().unwrap();
        ui.label(RichText::new(title).size(16.0).strong().color(TEXT));
        ui.add_space(6.0);
        ui.label(RichText::new(tr("API keys password")).small().color(MUTED));
        let r = ui.add_enabled(!busy, pw_edit(&mut p.password, ""));
        if !busy {
            r.request_focus();
        }
        if let Some(e) = &p.error {
            ui.label(RichText::new(e).color(RED));
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if busy {
                ui.spinner();
                ui.label(RichText::new(tr("Checking…")).color(MUTED));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(RichText::new(verb).color(theme::c::ON_ACCENT)).fill(ACCENT),
                    )
                    .clicked()
                {
                    go = true;
                }
                if ui.button(tr("Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
    });
    if cancel {
        app.api_ui.prompt = None;
    } else if go {
        app.keys_submit_prompt();
    }
}

fn change(ctx: &egui::Context, app: &mut App) {
    if app.api_ui.change.is_none() {
        return;
    }
    let busy = app.api_ui.busy();
    let esc = ctx.input(|i| i.key_pressed(Key::Escape));
    let (mut go, mut cancel) = (false, esc);
    modal(ctx, "api-key-change", 400.0, |ui| {
        let c = app.api_ui.change.as_mut().unwrap();
        ui.label(RichText::new(tr("Change the API keys password")).size(16.0).strong().color(TEXT));
        ui.add_space(8.0);
        ui.add_enabled(!busy, pw_edit(&mut c.current, tr("Current password")));
        ui.add_enabled(!busy, pw_edit(&mut c.new, tr("New password (8 characters or more)")));
        let r = ui.add_enabled(!busy, pw_edit(&mut c.confirm, tr("Confirm new password")));
        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !busy {
            go = true;
        }
        if let Some(e) = &c.error {
            ui.label(RichText::new(e).color(RED));
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if busy {
                ui.spinner();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(RichText::new(tr("Change password")).color(theme::c::ON_ACCENT))
                            .fill(ACCENT),
                    )
                    .clicked()
                {
                    go = true;
                }
                if ui.button(tr("Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
    });
    if cancel && !busy {
        app.api_ui.change = None;
    } else if go {
        app.keys_submit_change();
    }
}
