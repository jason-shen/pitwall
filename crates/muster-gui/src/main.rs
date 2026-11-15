//! muster — a standalone GPU terminal for running coding agents side by side.

mod app;
#[cfg(debug_assertions)]
mod devshot;
mod editors;
mod emoji;
mod find;
mod layout;
mod net;
mod settings;
mod term;
mod termview;
mod theme;
mod tiling;
mod views;

use std::path::PathBuf;

fn main() -> eframe::Result {
    // Project folder: first argument, else the current directory, else $HOME
    // (apps launched from the Dock start in `/`).
    let workspace = std::env::args()
        .nth(1)
        .map(|p| muster_core::paths::expand(&p))
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .filter(|d| d != &PathBuf::from("/"))
        })
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    let workspace = workspace.canonicalize().unwrap_or(workspace);

    // Read before the window exists: transparency can't be switched on later.
    let settings = settings::Settings::load();
    let transparent = settings.see_through;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("muster")
            .with_app_id("muster")
            .with_inner_size([1480.0, 920.0])
            .with_min_inner_size([760.0, 480.0])
            .with_transparent(transparent),
        renderer: renderer(),
        ..Default::default()
    };
    eframe::run_native(
        "muster",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::App::new(
                cc,
                workspace,
                settings,
                transparent,
            )))
        }),
    )
}

#[cfg(feature = "wgpu")]
fn renderer() -> eframe::Renderer {
    // With both compiled in, MUSTER_RENDERER=glow picks OpenGL.
    #[cfg(feature = "glow")]
    if std::env::var("MUSTER_RENDERER").as_deref() == Ok("glow") {
        return eframe::Renderer::Glow;
    }
    eframe::Renderer::Wgpu
}

#[cfg(all(feature = "glow", not(feature = "wgpu")))]
fn renderer() -> eframe::Renderer {
    eframe::Renderer::Glow
}
