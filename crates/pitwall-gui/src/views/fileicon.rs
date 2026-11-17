//! Icons for the file tree, painted rather than taken from a font: the bundled
//! fonts have no file-type glyphs, and a Nerd Font is only there when the user
//! picked one for the terminal.

use egui::{pos2, vec2, Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke};

use super::{ACCENT, GREEN, MUTED, RED};

/// Width every icon is centred in, so names line up whatever the icon.
pub const WIDTH: f32 = 18.0;

const BLUE: Color32 = Color32::from_rgb(0x5f, 0x9e, 0xe8);
const CYAN: Color32 = Color32::from_rgb(0x56, 0xc2, 0xd6);
const YELLOW: Color32 = Color32::from_rgb(0xe6, 0xc4, 0x5a);
const ORANGE: Color32 = Color32::from_rgb(0xe8, 0x8c, 0x4f);
const PURPLE: Color32 = Color32::from_rgb(0xb2, 0x8e, 0xe8);
const PINK: Color32 = Color32::from_rgb(0xe2, 0x7d, 0xb6);

/// The chip for a file name, or None for the plain page icon.
fn chip(name: &str) -> Option<(&'static str, Color32)> {
    let lower = name.to_ascii_lowercase();
    // Names that say more than their extension, or have none.
    let special = match lower.as_str() {
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep" => Some(("GIT", ORANGE)),
        "makefile" | "gnumakefile" | "justfile" => Some(("MK", ORANGE)),
        "license" | "licence" | "copying" | "license.md" | "license.txt" => {
            Some(("LIC", YELLOW))
        }
        ".env" => Some(("ENV", YELLOW)),
        n if n.starts_with(".env.") => Some(("ENV", YELLOW)),
        n if n.starts_with("dockerfile") || n == ".dockerignore" => Some(("DKR", CYAN)),
        _ => None,
    };
    if special.is_some() {
        return special;
    }
    // Path::extension treats ".bashrc" as having none; so does this.
    let ext = lower.rsplit_once('.').filter(|(stem, _)| !stem.is_empty())?.1;
    Some(match ext {
        "md" | "markdown" | "mdx" => ("MD", BLUE),
        "txt" | "rst" | "log" => ("TXT", MUTED),
        "rs" => ("RS", ORANGE),
        "ts" | "mts" | "cts" => ("TS", BLUE),
        "tsx" => ("TSX", BLUE),
        "js" | "mjs" | "cjs" => ("JS", YELLOW),
        "jsx" => ("JSX", YELLOW),
        "py" | "pyi" => ("PY", BLUE),
        "go" => ("GO", CYAN),
        "rb" => ("RB", RED),
        "java" => ("JV", ORANGE),
        "kt" | "kts" => ("KT", PURPLE),
        "swift" => ("SW", ORANGE),
        "c" => ("C", BLUE),
        "h" => ("H", PURPLE),
        "cpp" | "cc" | "cxx" => ("C++", BLUE),
        "hpp" | "hh" | "hxx" => ("H++", PURPLE),
        "cs" => ("C#", GREEN),
        "php" => ("PHP", PURPLE),
        "lua" => ("LUA", BLUE),
        "zig" => ("ZIG", ORANGE),
        "ex" | "exs" => ("EX", PURPLE),
        "sh" | "bash" | "zsh" | "fish" => ("SH", GREEN),
        "ps1" => ("PS", BLUE),
        "json" | "jsonc" | "json5" => ("{}", YELLOW),
        "toml" => ("TML", ORANGE),
        "yaml" | "yml" => ("YML", PURPLE),
        "ini" | "conf" | "cfg" => ("CFG", MUTED),
        "xml" | "plist" => ("XML", ORANGE),
        "html" | "htm" => ("<>", ORANGE),
        "css" => ("#", BLUE),
        "scss" | "sass" | "less" => ("#", PINK),
        "vue" => ("VUE", GREEN),
        "svelte" => ("SV", ORANGE),
        "sql" => ("SQL", CYAN),
        "graphql" | "gql" => ("GQL", PINK),
        "proto" => ("PB", CYAN),
        "tf" | "hcl" => ("TF", PURPLE),
        "nix" => ("NIX", BLUE),
        "wasm" => ("WSM", PURPLE),
        "diff" | "patch" => ("±", GREEN),
        "svg" => ("SVG", YELLOW),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "icns" | "avif" | "heic"
        | "tiff" => ("IMG", PURPLE),
        "pdf" => ("PDF", RED),
        "doc" | "docx" | "odt" | "rtf" | "pages" => ("DOC", BLUE),
        "xls" | "xlsx" | "ods" | "numbers" => ("XLS", GREEN),
        "csv" | "tsv" => ("CSV", GREEN),
        "ppt" | "pptx" | "key" => ("PPT", ORANGE),
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" => ("ZIP", MUTED),
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => ("AUD", PINK),
        "mp4" | "mov" | "mkv" | "webm" | "avi" => ("VID", PINK),
        "ttf" | "otf" | "woff" | "woff2" => ("FNT", MUTED),
        "lock" => ("LCK", MUTED),
        _ => return None,
    })
}

/// Draws the icon for `name` centred vertically on `left.y`, starting at `left.x`.
pub fn file(p: &Painter, left: Pos2, name: &str) {
    let c = pos2(left.x + WIDTH / 2.0, left.y);
    match chip(name) {
        Some((label, color)) => {
            let r = Rect::from_center_size(c, vec2(WIDTH, 12.0));
            p.rect_filled(r, 3.0, color.gamma_multiply(0.18));
            // Three letters only fit at the smaller size.
            let size = if label.chars().count() <= 2 { 8.5 } else { 7.0 };
            p.text(c, Align2::CENTER_CENTER, label, FontId::monospace(size), color);
        }
        None => {
            let r = Rect::from_center_size(c, vec2(10.0, 13.0));
            let fold = 3.5;
            let stroke = Stroke::new(1.0, MUTED);
            p.add(Shape::closed_line(
                vec![
                    r.left_top(),
                    pos2(r.right() - fold, r.top()),
                    pos2(r.right(), r.top() + fold),
                    r.right_bottom(),
                    r.left_bottom(),
                ],
                stroke,
            ));
            p.add(Shape::line(
                vec![
                    pos2(r.right() - fold, r.top()),
                    pos2(r.right() - fold, r.top() + fold),
                    pos2(r.right(), r.top() + fold),
                ],
                stroke,
            ));
        }
    }
}

pub fn folder(p: &Painter, left: Pos2, open: bool) {
    let (x, y) = (left.x + 1.0, left.y);
    let w = WIDTH - 2.0;
    let tab = Rect::from_min_size(pos2(x, y - 6.0), vec2(6.5, 3.0));
    let body = Rect::from_min_max(pos2(x, y - 4.5), pos2(x + w, y + 6.0));
    if open {
        let back = ACCENT.gamma_multiply(0.45);
        p.rect_filled(tab, 1.0, back);
        p.rect_filled(body, 1.5, back);
        // The front flap leans out to read as an open folder.
        p.add(Shape::convex_polygon(
            vec![
                pos2(x + 3.0, y - 1.0),
                pos2(x + w + 1.0, y - 1.0),
                pos2(x + w - 2.0, y + 6.0),
                pos2(x, y + 6.0),
            ],
            ACCENT.gamma_multiply(0.9),
            Stroke::NONE,
        ));
    } else {
        let fill = ACCENT.gamma_multiply(0.8);
        p.rect_filled(tab, 1.0, fill);
        p.rect_filled(body, 1.5, fill);
    }
}
