//! Colour emoji in terminals. egui can't read Apple Color Emoji (its glyphs
//! are bitmaps), and the font egui bundles predates newer emoji like 🦀. On
//! macOS each emoji is drawn once by CoreText into an image, as Terminal.app
//! draws them, and kept as a texture.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Emoji presentation: anything in the emoji planes, or forced by VS16.
pub fn is_emoji(s: &str) -> bool {
    s.chars().any(|c| {
        let u = c as u32;
        (0x1F000..=0x1FAFF).contains(&u) || u == 0xFE0F
    })
}

/// The Fitzpatrick skin-tone modifiers (🏻 to 🏿).
pub fn is_skin_tone(s: &str) -> bool {
    let mut it = s.chars();
    matches!((it.next(), it.next()), (Some(c), None) if ('\u{1F3FB}'..='\u{1F3FF}').contains(&c))
}

type Cache = Mutex<HashMap<(String, u32), Option<egui::TextureHandle>>>;

/// A texture of `s` about `px` physical pixels tall, or None where the
/// platform can't draw it.
pub fn texture(ctx: &egui::Context, s: &str, px: u32) -> Option<egui::TextureHandle> {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (s.to_string(), px);
    if let Some(t) = cache.lock().unwrap().get(&key) {
        return t.clone();
    }
    let tex = rasterize(s, px)
        .map(|img| ctx.load_texture(format!("emoji:{s}:{px}"), img, egui::TextureOptions::LINEAR));
    cache.lock().unwrap().insert(key, tex.clone());
    tex
}

#[cfg(not(target_os = "macos"))]
fn rasterize(_s: &str, _px: u32) -> Option<egui::ColorImage> {
    None
}

#[cfg(target_os = "macos")]
fn rasterize(s: &str, px: u32) -> Option<egui::ColorImage> {
    mac::rasterize(s, px)
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    type CFTypeRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithBytes(
            alloc: CFTypeRef,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> CFTypeRef;
        fn CFDictionaryCreate(
            alloc: CFTypeRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            n: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFTypeRef;
        fn CFAttributedStringCreate(alloc: CFTypeRef, s: CFTypeRef, attrs: CFTypeRef) -> CFTypeRef;
        fn CFRelease(cf: CFTypeRef);
        // Only their addresses are used.
        static kCFTypeDictionaryKeyCallBacks: u8;
        static kCFTypeDictionaryValueCallBacks: u8;
    }

    #[link(name = "CoreText", kind = "framework")]
    extern "C" {
        fn CTFontCreateWithName(name: CFTypeRef, size: f64, matrix: *const c_void) -> CFTypeRef;
        fn CTLineCreateWithAttributedString(s: CFTypeRef) -> CFTypeRef;
        fn CTLineGetTypographicBounds(
            line: CFTypeRef,
            ascent: *mut f64,
            descent: *mut f64,
            leading: *mut f64,
        ) -> f64;
        fn CTLineDraw(line: CFTypeRef, ctx: CFTypeRef);
        static kCTFontAttributeName: CFTypeRef;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGColorSpaceCreateDeviceRGB() -> CFTypeRef;
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: CFTypeRef,
            bitmap_info: u32,
        ) -> CFTypeRef;
        fn CGContextSetTextPosition(ctx: CFTypeRef, x: f64, y: f64);
    }

    const UTF8: u32 = 0x0800_0100;
    /// kCGImageAlphaPremultipliedLast: RGBA bytes, alpha premultiplied.
    const RGBA_PREMULTIPLIED: u32 = 1;

    pub fn rasterize(s: &str, px: u32) -> Option<egui::ColorImage> {
        if s.is_empty() || px == 0 {
            return None;
        }
        unsafe {
            let text =
                CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, UTF8, 0);
            let name =
                CFStringCreateWithBytes(std::ptr::null(), b"AppleColorEmoji".as_ptr(), 15, UTF8, 0);
            if text.is_null() || name.is_null() {
                return None;
            }
            // Emoji fill about their em square; leave a little room in the row.
            let font = CTFontCreateWithName(name, px as f64 * 0.82, std::ptr::null());
            let keys = [kCTFontAttributeName];
            let values = [font];
            let attrs = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks as *const u8 as *const c_void,
                &kCFTypeDictionaryValueCallBacks as *const u8 as *const c_void,
            );
            let astr = CFAttributedStringCreate(std::ptr::null(), text, attrs);
            let line = CTLineCreateWithAttributedString(astr);
            let (mut ascent, mut descent, mut leading) = (0.0, 0.0, 0.0);
            let width = CTLineGetTypographicBounds(line, &mut ascent, &mut descent, &mut leading);
            let w = (width.ceil() as usize).clamp(1, 4 * px as usize);
            let h = px as usize;
            let mut buf = vec![0u8; w * h * 4];
            let space = CGColorSpaceCreateDeviceRGB();
            let ctx = CGBitmapContextCreate(
                buf.as_mut_ptr().cast(),
                w,
                h,
                8,
                w * 4,
                space,
                RGBA_PREMULTIPLIED,
            );
            if !ctx.is_null() {
                // CoreGraphics' origin is bottom-left; centre the line vertically.
                let baseline = (h as f64 - (ascent + descent)) / 2.0 + descent;
                CGContextSetTextPosition(ctx, 0.0, baseline);
                CTLineDraw(line, ctx);
                CFRelease(ctx);
            }
            for r in [space, line, astr, attrs, font, name, text] {
                if !r.is_null() {
                    CFRelease(r);
                }
            }
            // Nothing drawn: CoreText had no glyph for it either.
            if ctx.is_null() || buf.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
                return None;
            }
            Some(egui::ColorImage::from_rgba_premultiplied([w, h], &buf))
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn draws_a_crab_in_colour() {
        let img = super::rasterize("🦀", 40).expect("CoreText drew nothing");
        assert_eq!(img.size[1], 40);
        // Some pixel is clearly red: it's the colour glyph, not an outline.
        assert!(img
            .pixels
            .iter()
            .any(|p| p.r() > 150 && p.g() < 120 && p.a() > 200));
    }

    #[test]
    fn what_counts_as_emoji() {
        assert!(super::is_emoji("🦀"));
        assert!(super::is_emoji("☁\u{fe0f}"));
        assert!(!super::is_emoji("☁"));
        assert!(!super::is_emoji("⏵"));
        assert!(!super::is_emoji("\u{e7a8}"));
        assert!(super::is_skin_tone("\u{1F3FD}"));
        assert!(!super::is_skin_tone("👍"));
    }
}
