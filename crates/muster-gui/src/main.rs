//! muster — a standalone GPU terminal for running coding agents side by side.

mod app;
mod briefs;
#[cfg(debug_assertions)]
mod devshot;
mod editors;
mod emoji;
mod find;
mod i18n;
mod keys;
mod layout;
mod markdown;
mod net;
mod settings;
mod term;
mod termview;
mod theme;
mod tiling;
mod trash;
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
    let (settings, config_problems) = settings::Settings::load();
    let transparent = settings.see_through;
    i18n::set(i18n::Lang::from_code(&settings.language));
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("muster")
        .with_app_id("muster")
        .with_inner_size([1480.0, 920.0])
        .with_min_inner_size([760.0, 480.0])
        .with_transparent(transparent);
    if transparent {
        // Over a clear window, macOS 27 draws the title bar with no backing at
        // all, so the content runs under it and paints the strip itself.
        viewport = viewport
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false);
    }
    // Muster.app has its .icns; this is for running it any other way (Linux,
    // cargo run), where the window would otherwise get a blank icon.
    if let Ok(icon) =
        eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/muster-256.png"))
    {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
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
                config_problems,
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
