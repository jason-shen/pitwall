//! Terminal fonts, colour schemes and the see-through backdrop. The font and
//! scheme code is shared with keel; keep fixes in step.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::Arc;

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Stroke};

use crate::settings::TermFont;

pub mod c {
    use egui::Color32;
    // Warm graphite with a sea-teal accent. The sidebar and tab strip sit darker
    // than the content area so the chrome recedes.
    pub const BG: Color32 = Color32::from_rgb(0x15, 0x16, 0x17);
    pub const SIDEBAR: Color32 = Color32::from_rgb(0x0f, 0x10, 0x11);
    pub const PANEL: Color32 = Color32::from_rgb(0x1b, 0x1c, 0x1e);
    pub const CARD: Color32 = Color32::from_rgb(0x1d, 0x1f, 0x21);
    pub const CARD_HOVER: Color32 = Color32::from_rgb(0x25, 0x28, 0x2a);
    pub const INPUT: Color32 = Color32::from_rgb(0x1a, 0x1b, 0x1d);
    pub const BORDER: Color32 = Color32::from_rgb(0x2d, 0x30, 0x32);
    pub const TEXT: Color32 = Color32::from_rgb(0xe9, 0xe7, 0xe3);
    pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9c, 0x99, 0x93);
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x65, 0x62, 0x5d);
    pub const ACCENT: Color32 = Color32::from_rgb(0x2b, 0xc4, 0xae);
    // White on the teal is too low-contrast, so filled accent surfaces use dark text.
    pub const ON_ACCENT: Color32 = Color32::from_rgb(0x07, 0x1f, 0x1b);
    pub const SUCCESS: Color32 = Color32::from_rgb(0x8c, 0xd0, 0x5c);
    pub const DANGER: Color32 = Color32::from_rgb(0xec, 0x5f, 0x5a);
    pub const WARNING: Color32 = Color32::from_rgb(0xf0, 0xa1, 0x3c);
}

pub fn apply_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = c::PANEL;
        v.window_fill = c::PANEL;
        v.extreme_bg_color = c::INPUT;
        v.faint_bg_color = c::CARD;
        v.override_text_color = Some(c::TEXT);
        v.hyperlink_color = c::ACCENT;
        v.selection.bg_fill = c::ACCENT.gamma_multiply(0.55);
        v.selection.stroke = Stroke::new(1.0, c::TEXT);
        v.window_stroke = Stroke::new(1.0, c::BORDER);
        v.slider_trailing_fill = true;
        v.window_corner_radius = CornerRadius::same(6);
        v.menu_corner_radius = CornerRadius::same(5);
        v.window_shadow = egui::epaint::Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(140),
        };
        v.popup_shadow = egui::epaint::Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: Color32::from_black_alpha(120),
        };
        let r = CornerRadius::same(4);
        v.widgets.noninteractive.bg_fill = c::CARD;
        v.widgets.noninteractive.weak_bg_fill = c::CARD;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, c::BORDER);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, c::TEXT_DIM);
        v.widgets.noninteractive.corner_radius = r;
        v.widgets.inactive.bg_fill = c::CARD;
        v.widgets.inactive.weak_bg_fill = c::CARD;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, c::BORDER);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, c::TEXT);
        v.widgets.inactive.corner_radius = r;
        v.widgets.hovered.bg_fill = c::CARD_HOVER;
        v.widgets.hovered.weak_bg_fill = c::CARD_HOVER;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, c::BORDER);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, c::TEXT);
        v.widgets.hovered.corner_radius = r;
        // egui also uses the pressed state's text colour for every .strong()
        // label, so a dark-on-teal pressed button made bold headings near
        // invisible. Pressed gets a teal tint with light text instead.
        v.widgets.active.bg_fill = c::ACCENT.gamma_multiply(0.35);
        v.widgets.active.weak_bg_fill = c::ACCENT.gamma_multiply(0.35);
        v.widgets.active.bg_stroke = Stroke::new(1.0, c::ACCENT);
        v.widgets.active.fg_stroke = Stroke::new(1.0, c::TEXT);
        v.widgets.active.corner_radius = r;
        v.widgets.open = v.widgets.hovered;

        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.window_margin = egui::Margin::same(20);
        style.spacing.menu_margin = egui::Margin::same(6);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.text_edit_width = 260.0;
        style.spacing.scroll = egui::style::ScrollStyle::floating();
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(11.5));
        style
            .text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(20.0));
        style
            .text_styles
            .insert(egui::TextStyle::Monospace, egui::FontId::monospace(13.0));
    });
}

// See-through mode, the way Ghostty does it: one tint (the scheme background at
// the opacity setting) is painted over whatever is behind the window once, and
// the chrome becomes faint overlays on it. Stacking separately translucent
// panels compounds their alpha and looks patchy. Dialogs stay solid.
static SEE_THROUGH: AtomicBool = AtomicBool::new(false);
static TINT: AtomicU32 = AtomicU32::new(0);
static TINT_ALPHA: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0

pub fn set_backdrop(see_through: bool, tint: Color32, alpha: f32) {
    SEE_THROUGH.store(see_through, Relaxed);
    TINT.store(u32::from_be_bytes(tint.to_array()), Relaxed);
    TINT_ALPHA.store(alpha.clamp(0.0, 1.0).to_bits(), Relaxed);
}

fn tint() -> Option<(Color32, f32)> {
    if !SEE_THROUGH.load(Relaxed) {
        return None;
    }
    let [r, g, b, _] = TINT.load(Relaxed).to_be_bytes();
    Some((
        Color32::from_rgb(r, g, b),
        f32::from_bits(TINT_ALPHA.load(Relaxed)),
    ))
}

/// The window-wide tint to paint first, if the window is see-through.
pub fn backdrop() -> Option<Color32> {
    tint().map(|(c, a)| c.gamma_multiply(a))
}

/// A chrome colour (tab bar, sidebar). Opaque normally; over the backdrop, a
/// light or dark overlay keeping roughly the same contrast against `base`.
pub fn surface(col: Color32, base: Color32) -> Color32 {
    if backdrop().is_none() {
        return col;
    }
    let luma = |c: Color32| (c.r() as f32 + c.g() as f32 + c.b() as f32) / 3.0;
    let (b, d) = (luma(base), luma(col) - luma(base));
    if d > 0.5 {
        Color32::from_white_alpha(((d / (255.0 - b)).min(0.06) * 255.0) as u8)
    } else if d < -0.5 {
        Color32::from_black_alpha(((-d / b.max(1.0)).min(0.22) * 255.0) as u8)
    } else {
        Color32::TRANSPARENT
    }
}

/// A terminal's default background: nothing to paint when the backdrop is
/// already this colour, otherwise the colour at the backdrop's opacity.
pub fn term_bg(scheme_bg: Color32) -> Color32 {
    match tint() {
        None => scheme_bg,
        Some((t, _)) if t == scheme_bg => Color32::TRANSPARENT,
        Some((_, a)) => scheme_bg.gamma_multiply(a),
    }
}

const TERM_FAMILY: &str = "terminal";

const TERM_BOLD_FAMILY: &str = "terminal-bold";

pub fn term_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, FontFamily::Name(TERM_FAMILY.into()))
}

/// The face for bold cells, when the font has one; otherwise None and the
/// terminal thickens regular text instead.
pub fn term_bold_font(size: f32) -> Option<egui::FontId> {
    REAL_BOLD
        .load(std::sync::atomic::Ordering::Relaxed)
        .then(|| egui::FontId::new(size, FontFamily::Name(TERM_BOLD_FAMILY.into())))
}

static REAL_BOLD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// One face of a font family, as far as picking a weight goes.
#[derive(Clone, Debug)]
struct Face {
    path: std::path::PathBuf,
    index: u32,
    weight: f32,
    /// The wght axis range, for a variable font.
    axis: Option<(f32, f32)>,
}

impl Face {
    /// Font data for this face at `weight`: a variable font is set to it, a
    /// static face is what it is.
    fn data(&self, weight: u16) -> Result<FontData, String> {
        let mut d = read_font(&self.path, self.index)?;
        if let Some((lo, hi)) = self.axis {
            d.tweak.coords = egui::epaint::text::VariationCoords::new([(
                b"wght",
                (weight as f32).clamp(lo, hi),
            )]);
        }
        Ok(d)
    }

    fn covers(&self, weight: u16) -> bool {
        match self.axis {
            Some((lo, hi)) => (lo..=hi).contains(&(weight as f32)),
            None => (self.weight - weight as f32).abs() < 1.0,
        }
    }
}

/// The upright faces of the family `path`/`index` belongs to: the rest of its
/// collection, plus files next to it sharing its name stem (JetBrainsMono-Bold.ttf
/// beside JetBrainsMono-Regular.ttf). Not a scan of the whole fonts folder, which
/// holds hundreds of megabytes of unrelated fonts.
fn family_faces(path: &std::path::Path, index: u32) -> Vec<Face> {
    use skrifa::{attribute::Style, MetadataProvider};
    let Ok(bytes) = std::fs::read(path) else {
        return vec![];
    };
    let Some(family) = skrifa::FontRef::from_index(&bytes, index)
        .ok()
        .and_then(|f| family_name(&f))
    else {
        return vec![];
    };
    let stem = path
        .file_stem()
        .map(|s| {
            s.to_string_lossy()
                .split(['-', '_'])
                .next()
                .unwrap_or("")
                .to_lowercase()
        })
        .unwrap_or_default();
    let mut files = vec![path.to_path_buf()];
    if let (Some(dir), false) = (path.parent(), stem.is_empty()) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let ext = p
                .extension()
                .map(|x| x.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if p != path && name.starts_with(&stem) && matches!(ext.as_str(), "ttf" | "otf" | "ttc")
            {
                files.push(p);
            }
        }
    }
    let mut out = vec![];
    for f in files {
        let Ok(bytes) = std::fs::read(&f) else {
            continue;
        };
        let Ok(file) = skrifa::raw::FileRef::new(&bytes) else {
            continue;
        };
        for (i, font) in file.fonts().enumerate() {
            let Ok(font) = font else { continue };
            if font.attributes().style != Style::Normal
                || family_name(&font).as_deref() != Some(family.as_str())
            {
                continue;
            }
            let axis = font
                .axes()
                .iter()
                .find(|a| a.tag() == skrifa::Tag::new(b"wght"))
                .map(|a| (a.min_value(), a.max_value()));
            out.push(Face {
                path: f.clone(),
                index: i as u32,
                weight: font.attributes().weight.value(),
                axis,
            });
        }
    }
    out
}

/// The face to draw `weight` with: a variable face covering it, else the
/// static face nearest to it.
fn pick(faces: &[Face], weight: u16) -> Option<&Face> {
    faces
        .iter()
        .find(|f| f.axis.is_some() && f.covers(weight))
        .or_else(|| {
            faces.iter().filter(|f| f.axis.is_none()).min_by(|a, b| {
                (a.weight - weight as f32)
                    .abs()
                    .total_cmp(&(b.weight - weight as f32).abs())
            })
        })
}

/// The weights worth offering for a font: every hundred a variable font spans,
/// or the weights its static faces come in.
pub fn available_weights(term: Option<&TermFont>) -> Vec<u16> {
    let Some((path, index)) = term
        .map(|t| (t.path.clone(), t.index))
        .or_else(default_mono)
    else {
        return vec![400, 700];
    };
    let mut w: Vec<u16> = vec![];
    for f in family_faces(&path, index) {
        match f.axis {
            Some((lo, hi)) => w.extend(
                (1..=9)
                    .map(|k| k * 100)
                    .filter(|&k| (lo..=hi).contains(&(k as f32))),
            ),
            None => w.push(f.weight.round() as u16),
        }
    }
    w.sort_unstable();
    w.dedup();
    if w.is_empty() {
        w = vec![400];
    }
    w
}

pub fn weight_name(w: u16) -> &'static str {
    crate::i18n::tr(match w {
        0..=149 => "Thin",
        150..=249 => "Extra Light",
        250..=349 => "Light",
        350..=449 => "Regular",
        450..=549 => "Medium",
        550..=649 => "Semibold",
        650..=749 => "Bold",
        750..=849 => "Extra Bold",
        _ => "Black",
    })
}

/// The system monospace font muster falls back to, which also gets the weights.
fn default_mono() -> Option<(std::path::PathBuf, u32)> {
    if let Ok(p) = std::env::var("MUSTER_FONT") {
        return Some((p.into(), 0));
    }
    MONO_CANDIDATES
        .iter()
        .map(std::path::Path::new)
        .find(|p| p.is_file())
        .map(|p| (p.to_path_buf(), 0))
}

const MONO_CANDIDATES: &[&str] = &[
    // macOS
    "/System/Library/Fonts/SFNSMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
    // Linux
    "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
];

/// Fallbacks for glyphs the terminal font lacks: symbols, box drawing, CJK.
const FALLBACK_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Apple Symbols.ttf",
    // Ahead of STIX, which has a stray の that otherwise comes out small
    // and high in Japanese text.
    "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
    // What macOS itself falls back to for ⏵ ⏸ ⏺ (Claude Code's mode line)
    // and ⎿ (under its tool output).
    "/System/Library/Fonts/Supplemental/STIXTwoMath.otf",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/AppleSDGothicNeo.ttc",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];

/// Han characters look different in Chinese and Japanese fonts, and the
/// fallback list reaches the Japanese one first. For Chinese and Korean UI the
/// matching system font moves ahead of the others in the UI family. The
/// terminal chain is left alone.
fn prefer_cjk(fonts: &mut FontDefinitions, lang: crate::i18n::Lang) {
    use crate::i18n::Lang;
    let first: &[&str] = match lang {
        Lang::Zh => &[
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
        ],
        Lang::Ko => &["/System/Library/Fonts/AppleSDGothicNeo.ttc"],
        _ => return,
    };
    let Some(chain) = fonts.families.get_mut(&FontFamily::Proportional) else {
        return;
    };
    let names: Vec<String> = first
        .iter()
        .map(|p| format!("sys:{p}"))
        .filter(|n| fonts.font_data.contains_key(n))
        .collect();
    chain.retain(|n| !names.contains(n));
    let at = chain
        .iter()
        .position(|n| n.starts_with("sys:"))
        .unwrap_or(chain.len());
    chain.splice(at..at, names);
}

/// egui's built-in fonts plus system fallbacks, loaded once. `apply` layers the
/// chosen terminal font on top whenever it changes.
pub struct FontSet {
    base: FontDefinitions,
}

impl FontSet {
    pub fn load() -> Self {
        let mut fonts = FontDefinitions::default();
        let mut fallbacks = vec![];
        for p in FALLBACK_CANDIDATES {
            let Ok(data) = read_font(std::path::Path::new(p), 0) else {
                continue;
            };
            let name = format!("sys:{p}");
            fonts.font_data.insert(name.clone(), Arc::new(data));
            fallbacks.push(name);
        }
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .extend(fallbacks.iter().cloned());
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .extend(fallbacks);
        Self { base: fonts }
    }

    /// Install fonts with `term` (or the system monospace font) first in the
    /// terminal family at `weight`, and its `bold` weight in a family of its own.
    /// On failure the terminal falls back to the default monospace chain and the
    /// error is returned.
    pub fn apply(
        &self,
        ctx: &egui::Context,
        term: Option<&TermFont>,
        weight: u16,
        bold: u16,
        lang: crate::i18n::Lang,
    ) -> Result<(), String> {
        let mut fonts = self.base.clone();
        prefer_cjk(&mut fonts, lang);
        let fallback = fonts
            .families
            .get(&FontFamily::Monospace)
            .cloned()
            .unwrap_or_default();
        let mut chain = fallback.clone();
        let mut bold_chain = None;
        let mut result = Ok(());
        let source = term
            .map(|t| (t.path.clone(), t.index))
            .or_else(default_mono);
        if let Some((path, index)) = source {
            let faces = family_faces(&path, index);
            let regular = pick(&faces, weight).cloned();
            match regular
                .as_ref()
                .map(|f| f.data(weight))
                .unwrap_or_else(|| read_font(&path, index))
            {
                Ok(data) => {
                    fonts.font_data.insert("term-font".into(), Arc::new(data));
                    chain.insert(0, "term-font".into());
                }
                Err(e) => {
                    result = Err(crate::i18n::trf!(
                        "Couldn't load {path}: {e}",
                        path = path.display(),
                        e = e
                    ))
                }
            }
            // Only a face that's actually heavier counts; one that would come
            // out the same as regular leaves bold to the thickening fallback.
            if let Some(b) = pick(&faces, bold).filter(|b| {
                b.axis.is_some() || regular.as_ref().is_none_or(|r| b.weight > r.weight)
            }) {
                if let Ok(data) = b.data(bold) {
                    fonts.font_data.insert("term-bold".into(), Arc::new(data));
                    let mut c = fallback.clone();
                    c.insert(0, "term-bold".into());
                    bold_chain = Some(c);
                }
            }
        }
        REAL_BOLD.store(bold_chain.is_some() && result.is_ok(), Relaxed);
        fonts
            .families
            .insert(FontFamily::Name(TERM_FAMILY.into()), chain.clone());
        fonts.families.insert(
            FontFamily::Name(TERM_BOLD_FAMILY.into()),
            bold_chain.unwrap_or(chain),
        );
        align_cjk_baselines(&mut fonts);
        ctx.set_fonts(fonts);
        result
    }
}

/// egui places a glyph from a fallback font by centring that font's line box
/// on the primary's, not by putting it on the primary's baseline. CJK fonts
/// have much taller line boxes than Latin ones, so Han and kana glyphs rode
/// visibly high next to Latin text. Each family gets its own copy of every CJK
/// fallback, shifted down onto the baseline of that family's primary font.
/// Symbol and box-drawing fallbacks are left as they were.
fn align_cjk_baselines(fonts: &mut FontDefinitions) {
    let families: Vec<FontFamily> = fonts.families.keys().cloned().collect();
    for family in families {
        let chain = fonts.families[&family].clone();
        let Some(primary) = chain.first().and_then(|n| fonts.font_data.get(n)) else {
            continue;
        };
        let Some(p) = em_metrics(primary) else {
            continue;
        };
        let mut out = Vec::with_capacity(chain.len());
        for name in chain {
            let shifted = fonts
                .font_data
                .get(&name)
                .filter(|_| name.starts_with("sys:"))
                .filter(|d| is_cjk(d))
                .and_then(|d| Some((d, em_metrics(d)?)))
                .map(|(d, f)| (d, p.ascent - f.ascent - 0.5 * (p.height - f.height)))
                .filter(|(_, shift)| shift.abs() > 0.005);
            match shifted {
                Some((d, shift)) => {
                    let mut d = (**d).clone();
                    d.tweak.y_offset_factor += shift;
                    let key = format!("{name}@{shift:.3}");
                    fonts.font_data.insert(key.clone(), Arc::new(d));
                    out.push(key);
                }
                None => out.push(name),
            }
        }
        fonts.families.insert(family, out);
    }
}

struct EmMetrics {
    ascent: f32,
    /// What egui takes as the row height: ascent - descent + line gap.
    height: f32,
}

/// The same vertical metrics egui reads (skrifa's, at the default location).
fn em_metrics(d: &FontData) -> Option<EmMetrics> {
    use skrifa::{instance::LocationRef, instance::Size, MetadataProvider};
    let font = skrifa::FontRef::from_index((*d.font).as_ref(), d.index).ok()?;
    let m = font.metrics(Size::unscaled(), LocationRef::default());
    let upem = m.units_per_em as f32;
    (upem > 0.0).then(|| EmMetrics {
        ascent: m.ascent / upem,
        height: (m.ascent - m.descent + m.leading) / upem,
    })
}

fn is_cjk(d: &FontData) -> bool {
    use skrifa::MetadataProvider;
    skrifa::FontRef::from_index((*d.font).as_ref(), d.index)
        .map(|f| {
            let cmap = f.charmap();
            ['中', 'あ', '한'].iter().any(|&c| cmap.map(c).is_some())
        })
        .unwrap_or(false)
}

fn read_font(path: &std::path::Path, index: u32) -> Result<FontData, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    skrifa::FontRef::from_index(&bytes, index)
        .map_err(|e| crate::i18n::trf!("not a usable font ({e})", e = e))?;
    let mut data = FontData::from_owned(bytes);
    data.index = index;
    Ok(data)
}

/// Parse a font file the user pointed at by hand, naming it from its own metadata.
pub fn term_font_from_path(path: &std::path::Path) -> Result<TermFont, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let file = skrifa::raw::FileRef::new(&bytes)
        .map_err(|e| crate::i18n::trf!("not a font file ({e})", e = e))?;
    let font = file
        .fonts()
        .next()
        .and_then(|f| f.ok())
        .ok_or(crate::i18n::tr("no usable face in file"))?;
    let name = family_name(&font).unwrap_or_else(|| {
        path.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    Ok(TermFont {
        name,
        path: path.to_path_buf(),
        index: 0,
    })
}

fn family_name(font: &skrifa::FontRef) -> Option<String> {
    use skrifa::{string::StringId, MetadataProvider};
    [StringId::TYPOGRAPHIC_FAMILY_NAME, StringId::FAMILY_NAME]
        .into_iter()
        .find_map(|id| font.localized_strings(id).english_or_first())
        // macOS hides system faces behind a leading dot (".SF NS Mono").
        .map(|s| s.to_string().trim_start_matches('.').to_string())
        .filter(|s| !s.trim().is_empty())
}

/// Monospace fonts installed on this machine, one upright regular-ish face per
/// family, sorted by name. Reads every font file in the usual directories, so
/// call it off the UI thread.
pub fn scan_mono_fonts() -> Vec<TermFont> {
    use skrifa::{attribute::Style, MetadataProvider};

    let mut dirs: Vec<std::path::PathBuf> = vec![];
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if cfg!(target_os = "macos") {
        dirs.extend(["/System/Library/Fonts", "/Library/Fonts"].map(Into::into));
        dirs.extend(home.iter().map(|h| h.join("Library/Fonts")));
    } else {
        dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(Into::into));
        dirs.extend(
            home.iter()
                .flat_map(|h| [h.join(".local/share/fonts"), h.join(".fonts")]),
        );
    }

    let mut files = vec![];
    let mut stack: Vec<(std::path::PathBuf, u8)> = dirs.into_iter().map(|d| (d, 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                if depth < 4 {
                    stack.push((p, depth + 1));
                }
                continue;
            }
            let ext = p
                .extension()
                .map(|x| x.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            // The huge files are CJK and emoji fonts, never monospace, and reading them dominates the scan.
            if matches!(ext.as_str(), "ttf" | "otf" | "ttc") && meta.len() < 25 << 20 {
                files.push(p);
            }
        }
    }

    // family -> (score, font); lower score is closer to an upright 400 weight.
    let mut best: std::collections::HashMap<String, (f32, TermFont)> = Default::default();
    for path in files {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(file) = skrifa::raw::FileRef::new(&bytes) else {
            continue;
        };
        for (index, font) in file.fonts().enumerate() {
            let Ok(font) = font else { continue };
            if !is_monospace(&font) {
                continue;
            }
            let Some(name) = family_name(&font) else {
                continue;
            };
            let attrs = font.attributes();
            let score = (attrs.weight.value() - 400.0).abs()
                + if attrs.style == Style::Normal {
                    0.0
                } else {
                    1000.0
                };
            let entry = TermFont {
                name: name.clone(),
                path: path.clone(),
                index: index as u32,
            };
            match best.get(&name) {
                Some((s, _)) if *s <= score => {}
                _ => {
                    best.insert(name, (score, entry));
                }
            }
        }
    }
    let mut out: Vec<_> = best.into_values().map(|(_, f)| f).collect();
    out.sort_by_key(|f| f.name.to_lowercase());
    out
}

/// Plenty of monospace fonts don't set the post table's fixed-pitch flag, so
/// fall back to comparing advance widths of a narrow and a wide glyph.
fn is_monospace(font: &skrifa::FontRef) -> bool {
    use skrifa::{
        instance::{LocationRef, Size},
        raw::TableProvider,
        MetadataProvider,
    };
    let cmap = font.charmap();
    // Bitmap-only faces have no outlines for egui to draw; panose family 5 is
    // pictorial (Webdings and friends), which claim fixed pitch too.
    let Some(m) = cmap.map('M') else { return false };
    if font.outline_glyphs().get(m).is_none()
        || font
            .os2()
            .map(|o| o.panose_10().first() == Some(&5))
            .unwrap_or(false)
    {
        return false;
    }
    if font
        .post()
        .map(|p| p.is_fixed_pitch() != 0)
        .unwrap_or(false)
    {
        return true;
    }
    let metrics = font.glyph_metrics(Size::unscaled(), LocationRef::default());
    let adv = |c: char| cmap.map(c).and_then(|g| metrics.advance_width(g));
    match (adv('i'), adv('M'), adv('0')) {
        (Some(i), Some(m), Some(z)) => i > 0.0 && i == m && m == z,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Terminal colour schemes
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Scheme {
    pub name: &'static str,
    pub bg: Color32,
    pub fg: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    pub ansi: [Color32; 16],
}

const fn h(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}
pub const SCHEMES: &[Scheme] = &[
    Scheme {
        name: "Muster Dark",
        bg: h(0x131415),
        fg: h(0xe2dfda),
        cursor: h(0x2bc4ae),
        selection: h(0x264440),
        ansi: [
            h(0x232527),
            h(0xec5f5a),
            h(0x8cd05c),
            h(0xf0c062),
            h(0x5c9eef),
            h(0xc98bd9),
            h(0x2bc4ae),
            h(0xc9c5bf),
            h(0x65625d),
            h(0xff8580),
            h(0xaae682),
            h(0xffd88c),
            h(0x86baff),
            h(0xe0aaec),
            h(0x6fe0cf),
            h(0xffffff),
        ],
    },
    Scheme {
        name: "Dracula",
        bg: h(0x282a36),
        fg: h(0xf8f8f2),
        cursor: h(0xf8f8f2),
        selection: h(0x44475a),
        ansi: [
            h(0x21222c),
            h(0xff5555),
            h(0x50fa7b),
            h(0xf1fa8c),
            h(0xbd93f9),
            h(0xff79c6),
            h(0x8be9fd),
            h(0xf8f8f2),
            h(0x6272a4),
            h(0xff6e6e),
            h(0x69ff94),
            h(0xffffa5),
            h(0xd6acff),
            h(0xff92df),
            h(0xa4ffff),
            h(0xffffff),
        ],
    },
    Scheme {
        name: "Monokai",
        bg: h(0x272822),
        fg: h(0xf8f8f2),
        cursor: h(0xf8f8f0),
        selection: h(0x49483e),
        ansi: [
            h(0x272822),
            h(0xf92672),
            h(0xa6e22e),
            h(0xf4bf75),
            h(0x66d9ef),
            h(0xae81ff),
            h(0xa1efe4),
            h(0xf8f8f2),
            h(0x75715e),
            h(0xf92672),
            h(0xa6e22e),
            h(0xf4bf75),
            h(0x66d9ef),
            h(0xae81ff),
            h(0xa1efe4),
            h(0xf9f8f5),
        ],
    },
    Scheme {
        name: "Solarized Dark",
        bg: h(0x002b36),
        fg: h(0x839496),
        cursor: h(0x93a1a1),
        selection: h(0x073642),
        ansi: [
            h(0x073642),
            h(0xdc322f),
            h(0x859900),
            h(0xb58900),
            h(0x268bd2),
            h(0xd33682),
            h(0x2aa198),
            h(0xeee8d5),
            h(0x002b36),
            h(0xcb4b16),
            h(0x586e75),
            h(0x657b83),
            h(0x839496),
            h(0x6c71c4),
            h(0x93a1a1),
            h(0xfdf6e3),
        ],
    },
    Scheme {
        name: "Nord",
        bg: h(0x2e3440),
        fg: h(0xd8dee9),
        cursor: h(0xd8dee9),
        selection: h(0x434c5e),
        ansi: [
            h(0x3b4252),
            h(0xbf616a),
            h(0xa3be8c),
            h(0xebcb8b),
            h(0x81a1c1),
            h(0xb48ead),
            h(0x88c0d0),
            h(0xe5e9f0),
            h(0x4c566a),
            h(0xbf616a),
            h(0xa3be8c),
            h(0xebcb8b),
            h(0x81a1c1),
            h(0xb48ead),
            h(0x8fbcbb),
            h(0xeceff4),
        ],
    },
    Scheme {
        name: "Gruvbox Dark",
        bg: h(0x282828),
        fg: h(0xebdbb2),
        cursor: h(0xebdbb2),
        selection: h(0x504945),
        ansi: [
            h(0x282828),
            h(0xcc241d),
            h(0x98971a),
            h(0xd79921),
            h(0x458588),
            h(0xb16286),
            h(0x689d6a),
            h(0xa89984),
            h(0x928374),
            h(0xfb4934),
            h(0xb8bb26),
            h(0xfabd2f),
            h(0x83a598),
            h(0xd3869b),
            h(0x8ec07c),
            h(0xebdbb2),
        ],
    },
    Scheme {
        name: "One Dark",
        bg: h(0x282c34),
        fg: h(0xabb2bf),
        cursor: h(0x528bff),
        selection: h(0x3e4451),
        ansi: [
            h(0x3f4451),
            h(0xe06c75),
            h(0x98c379),
            h(0xe5c07b),
            h(0x61afef),
            h(0xc678dd),
            h(0x56b6c2),
            h(0xabb2bf),
            h(0x5c6370),
            h(0xe06c75),
            h(0x98c379),
            h(0xe5c07b),
            h(0x61afef),
            h(0xc678dd),
            h(0x56b6c2),
            h(0xffffff),
        ],
    },
    Scheme {
        name: "Tokyo Night",
        bg: h(0x1a1b26),
        fg: h(0xc0caf5),
        cursor: h(0xc0caf5),
        selection: h(0x33467c),
        ansi: [
            h(0x15161e),
            h(0xf7768e),
            h(0x9ece6a),
            h(0xe0af68),
            h(0x7aa2f7),
            h(0xbb9af7),
            h(0x7dcfff),
            h(0xa9b1d6),
            h(0x414868),
            h(0xf7768e),
            h(0x9ece6a),
            h(0xe0af68),
            h(0x7aa2f7),
            h(0xbb9af7),
            h(0x7dcfff),
            h(0xc0caf5),
        ],
    },
    Scheme {
        name: "Catppuccin Mocha",
        bg: h(0x1e1e2e),
        fg: h(0xcdd6f4),
        cursor: h(0xf5e0dc),
        selection: h(0x585b70),
        ansi: [
            h(0x45475a),
            h(0xf38ba8),
            h(0xa6e3a1),
            h(0xf9e2af),
            h(0x89b4fa),
            h(0xf5c2e7),
            h(0x94e2d5),
            h(0xbac2de),
            h(0x585b70),
            h(0xf38ba8),
            h(0xa6e3a1),
            h(0xf9e2af),
            h(0x89b4fa),
            h(0xf5c2e7),
            h(0x94e2d5),
            h(0xa6adc8),
        ],
    },
    Scheme {
        name: "Catppuccin Macchiato",
        bg: h(0x24273a),
        fg: h(0xcad3f5),
        cursor: h(0xf4dbd6),
        selection: h(0x5b6078),
        ansi: [
            h(0x494d64),
            h(0xed8796),
            h(0xa6da95),
            h(0xeed49f),
            h(0x8aadf4),
            h(0xf5bde6),
            h(0x8bd5ca),
            h(0xb8c0e0),
            h(0x5b6078),
            h(0xed8796),
            h(0xa6da95),
            h(0xeed49f),
            h(0x8aadf4),
            h(0xf5bde6),
            h(0x8bd5ca),
            h(0xa5adcb),
        ],
    },
    Scheme {
        name: "Catppuccin Frappé",
        bg: h(0x303446),
        fg: h(0xc6d0f5),
        cursor: h(0xf2d5cf),
        selection: h(0x626880),
        ansi: [
            h(0x51576d),
            h(0xe78284),
            h(0xa6d189),
            h(0xe5c890),
            h(0x8caaee),
            h(0xf4b8e4),
            h(0x81c8be),
            h(0xb5bfe2),
            h(0x626880),
            h(0xe78284),
            h(0xa6d189),
            h(0xe5c890),
            h(0x8caaee),
            h(0xf4b8e4),
            h(0x81c8be),
            h(0xa5adce),
        ],
    },
    Scheme {
        name: "Rosé Pine",
        bg: h(0x191724),
        fg: h(0xe0def4),
        cursor: h(0xe0def4),
        selection: h(0x403d52),
        ansi: [
            h(0x26233a),
            h(0xeb6f92),
            h(0x31748f),
            h(0xf6c177),
            h(0x9ccfd8),
            h(0xc4a7e7),
            h(0xebbcba),
            h(0xe0def4),
            h(0x6e6a86),
            h(0xeb6f92),
            h(0x31748f),
            h(0xf6c177),
            h(0x9ccfd8),
            h(0xc4a7e7),
            h(0xebbcba),
            h(0xe0def4),
        ],
    },
    Scheme {
        name: "Kanagawa",
        bg: h(0x1f1f28),
        fg: h(0xdcd7ba),
        cursor: h(0xc8c093),
        selection: h(0x2d4f67),
        ansi: [
            h(0x090618),
            h(0xc34043),
            h(0x76946a),
            h(0xc0a36e),
            h(0x7e9cd8),
            h(0x957fb8),
            h(0x6a9589),
            h(0xc8c093),
            h(0x727169),
            h(0xe82424),
            h(0x98bb6c),
            h(0xe6c384),
            h(0x7fb4ca),
            h(0x938aa9),
            h(0x7aa89f),
            h(0xdcd7ba),
        ],
    },
    Scheme {
        name: "Everforest Dark",
        bg: h(0x2d353b),
        fg: h(0xd3c6aa),
        cursor: h(0xd3c6aa),
        selection: h(0x475258),
        ansi: [
            h(0x475258),
            h(0xe67e80),
            h(0xa7c080),
            h(0xdbbc7f),
            h(0x7fbbb3),
            h(0xd699b6),
            h(0x83c092),
            h(0xd3c6aa),
            h(0x859289),
            h(0xe67e80),
            h(0xa7c080),
            h(0xdbbc7f),
            h(0x7fbbb3),
            h(0xd699b6),
            h(0x83c092),
            h(0xe6e2cc),
        ],
    },
    Scheme {
        name: "Tomorrow Night",
        bg: h(0x1d1f21),
        fg: h(0xc5c8c6),
        cursor: h(0xc5c8c6),
        selection: h(0x373b41),
        ansi: [
            h(0x282a2e),
            h(0xcc6666),
            h(0xb5bd68),
            h(0xf0c674),
            h(0x81a2be),
            h(0xb294bb),
            h(0x8abeb7),
            h(0xc5c8c6),
            h(0x969896),
            h(0xd54e53),
            h(0xb9ca4a),
            h(0xe7c547),
            h(0x7aa6da),
            h(0xc397d8),
            h(0x70c0b1),
            h(0xffffff),
        ],
    },
    Scheme {
        name: "GitHub Dark",
        bg: h(0x0d1117),
        fg: h(0xe6edf3),
        cursor: h(0x2f81f7),
        selection: h(0x264f78),
        ansi: [
            h(0x484f58),
            h(0xff7b72),
            h(0x3fb950),
            h(0xd29922),
            h(0x58a6ff),
            h(0xbc8cff),
            h(0x39c5cf),
            h(0xb1bac4),
            h(0x6e7681),
            h(0xffa198),
            h(0x56d364),
            h(0xe3b341),
            h(0x79c0ff),
            h(0xd2a8ff),
            h(0x56d4dd),
            h(0xffffff),
        ],
    },
    Scheme {
        name: "Night Owl",
        bg: h(0x011627),
        fg: h(0xd6deeb),
        cursor: h(0x80a4c2),
        selection: h(0x1d3b53),
        ansi: [
            h(0x011627),
            h(0xef5350),
            h(0x22da6e),
            h(0xaddb67),
            h(0x82aaff),
            h(0xc792ea),
            h(0x21c7a8),
            h(0xffffff),
            h(0x575656),
            h(0xef5350),
            h(0x22da6e),
            h(0xffeb95),
            h(0x82aaff),
            h(0xc792ea),
            h(0x7fdbca),
            h(0xffffff),
        ],
    },
    Scheme {
        name: "Ayu Dark",
        bg: h(0x0b0e14),
        fg: h(0xbfbdb6),
        cursor: h(0xe6b450),
        selection: h(0x273747),
        ansi: [
            h(0x01060e),
            h(0xea6c73),
            h(0x7fd962),
            h(0xf9af4f),
            h(0x53bdfa),
            h(0xcda1fa),
            h(0x90e1c6),
            h(0xc7c7c7),
            h(0x686868),
            h(0xf07178),
            h(0xaad94c),
            h(0xffb454),
            h(0x59c2ff),
            h(0xd2a6ff),
            h(0x95e6cb),
            h(0xffffff),
        ],
    },
    Scheme {
        name: "Snazzy",
        bg: h(0x282a36),
        fg: h(0xeff0eb),
        cursor: h(0x97979b),
        selection: h(0x43454f),
        ansi: [
            h(0x282a36),
            h(0xff5c57),
            h(0x5af78e),
            h(0xf3f99d),
            h(0x57c7ff),
            h(0xff6ac1),
            h(0x9aedfe),
            h(0xf1f1f0),
            h(0x686868),
            h(0xff5c57),
            h(0x5af78e),
            h(0xf3f99d),
            h(0x57c7ff),
            h(0xff6ac1),
            h(0x9aedfe),
            h(0xeff0eb),
        ],
    },
    Scheme {
        name: "Zenburn",
        bg: h(0x3f3f3f),
        fg: h(0xdcdccc),
        cursor: h(0x73635a),
        selection: h(0x21322f),
        ansi: [
            h(0x4d4d4d),
            h(0x705050),
            h(0x60b48a),
            h(0xf0dfaf),
            h(0x506070),
            h(0xdc8cc3),
            h(0x8cd0d3),
            h(0xdcdccc),
            h(0x709080),
            h(0xdca3a3),
            h(0xc3bf9f),
            h(0xe0cf9f),
            h(0x94bff3),
            h(0xec93d3),
            h(0x93e0e3),
            h(0xffffff),
        ],
    },
    Scheme {
        name: "Homebrew",
        bg: h(0x000000),
        fg: h(0x00ff00),
        cursor: h(0x23ff18),
        selection: h(0x083905),
        ansi: [
            h(0x000000),
            h(0x990000),
            h(0x00a600),
            h(0x999900),
            h(0x0000b2),
            h(0xb200b2),
            h(0x00a6b2),
            h(0xbfbfbf),
            h(0x666666),
            h(0xe50000),
            h(0x00d900),
            h(0xe5e500),
            h(0x0000ff),
            h(0xe500e5),
            h(0x00e5e5),
            h(0xe5e5e5),
        ],
    },
    Scheme {
        name: "One Light",
        bg: h(0xfafafa),
        fg: h(0x383a42),
        cursor: h(0x526eff),
        selection: h(0xd7dcf5),
        ansi: [
            h(0x383a42),
            h(0xe45649),
            h(0x50a14f),
            h(0xc18401),
            h(0x4078f2),
            h(0xa626a4),
            h(0x0184bc),
            h(0xa0a1a7),
            h(0x696c77),
            h(0xe45649),
            h(0x50a14f),
            h(0xc18401),
            h(0x4078f2),
            h(0xa626a4),
            h(0x0184bc),
            h(0xfafafa),
        ],
    },
    Scheme {
        name: "Solarized Light",
        bg: h(0xfdf6e3),
        fg: h(0x657b83),
        cursor: h(0x586e75),
        selection: h(0xeee8d5),
        ansi: [
            h(0x073642),
            h(0xdc322f),
            h(0x859900),
            h(0xb58900),
            h(0x268bd2),
            h(0xd33682),
            h(0x2aa198),
            h(0xeee8d5),
            h(0x002b36),
            h(0xcb4b16),
            h(0x586e75),
            h(0x657b83),
            h(0x839496),
            h(0x6c71c4),
            h(0x93a1a1),
            h(0xfdf6e3),
        ],
    },
    Scheme {
        name: "Gruvbox Light",
        bg: h(0xfbf1c7),
        fg: h(0x3c3836),
        cursor: h(0x3c3836),
        selection: h(0xd5c4a1),
        ansi: [
            h(0xfbf1c7),
            h(0xcc241d),
            h(0x98971a),
            h(0xd79921),
            h(0x458588),
            h(0xb16286),
            h(0x689d6a),
            h(0x7c6f64),
            h(0x928374),
            h(0x9d0006),
            h(0x79740e),
            h(0xb57614),
            h(0x076678),
            h(0x8f3f71),
            h(0x427b58),
            h(0x3c3836),
        ],
    },
    Scheme {
        name: "GitHub Light",
        bg: h(0xffffff),
        fg: h(0x1f2328),
        cursor: h(0x0969da),
        selection: h(0xb6e3ff),
        ansi: [
            h(0x24292f),
            h(0xcf222e),
            h(0x116329),
            h(0x4d2d00),
            h(0x0969da),
            h(0x8250df),
            h(0x1b7c83),
            h(0x6e7781),
            h(0x57606a),
            h(0xa40e26),
            h(0x1a7f37),
            h(0x633c01),
            h(0x218bff),
            h(0xa475f9),
            h(0x3192aa),
            h(0x8c959f),
        ],
    },
    Scheme {
        name: "Catppuccin Latte",
        bg: h(0xeff1f5),
        fg: h(0x4c4f69),
        cursor: h(0xdc8a78),
        selection: h(0xacb0be),
        ansi: [
            h(0x5c5f77),
            h(0xd20f39),
            h(0x40a02b),
            h(0xdf8e1d),
            h(0x1e66f5),
            h(0xea76cb),
            h(0x179299),
            h(0xacb0be),
            h(0x6c6f85),
            h(0xd20f39),
            h(0x40a02b),
            h(0xdf8e1d),
            h(0x1e66f5),
            h(0xea76cb),
            h(0x179299),
            h(0xbcc0cc),
        ],
    },
];

pub fn scheme(name: &str) -> &'static Scheme {
    SCHEMES
        .iter()
        .find(|s| s.name == name)
        .unwrap_or(&SCHEMES[0])
}

impl Scheme {
    pub fn indexed(&self, i: u8) -> Color32 {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                let i = i - 16;
                let lv = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Color32::from_rgb(lv(i / 36), lv((i / 6) % 6), lv(i % 6))
            }
            _ => {
                let g = 8 + (i - 232) * 10;
                Color32::from_rgb(g, g, g)
            }
        }
    }
}
