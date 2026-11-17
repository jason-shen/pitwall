//! Nerd Font icons for the file tree. Code points are from Nerd Fonts 3.4's
//! glyphnames.json (names in the comments), matching the bundled font.

use egui::{pos2, Align2, Color32, Painter, Pos2};

use super::{ACCENT, MUTED};
use crate::theme;

/// Width every icon is centred in, so names line up whatever the glyph.
pub const WIDTH: f32 = 18.0;

const fn hex(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

// seti-default
const PLAIN: (char, Color32) = ('\u{e64e}', MUTED);

fn icon(name: &str) -> (char, Color32) {
    let lower = name.to_ascii_lowercase();
    let special = match lower.as_str() {
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep" => {
            Some(('\u{e65d}', hex(0xf14c28))) // seti-git
        }
        "makefile" | "gnumakefile" | "justfile" => Some(('\u{e673}', hex(0xe37933))), // seti-makefile
        "license" | "licence" | "copying" | "license.md" | "license.txt" => {
            Some(('\u{e60a}', hex(0xd0bf41))) // seti-license
        }
        "package.json" | "package-lock.json" | ".npmrc" => Some(('\u{e616}', hex(0xe8274b))), // seti-npm
        "yarn.lock" => Some(('\u{e6a7}', hex(0x2c8ebb))), // seti-yarn
        "cargo.toml" | "cargo.lock" => Some(('\u{e68b}', hex(0xdea584))), // seti-rust
        n if n == ".env" || n.starts_with(".env.") => Some(('\u{e615}', hex(0xfaf743))), // seti-config
        n if n.starts_with("dockerfile") || n == ".dockerignore" || n.starts_with("docker-compose") => {
            Some(('\u{e650}', hex(0x458ee6))) // seti-docker
        }
        n if n.starts_with("build.gradle") => Some(('\u{e660}', hex(0x4a9bc4))), // seti-gradle
        _ => None,
    };
    if let Some(s) = special {
        return s;
    }
    // ".bashrc" has no extension, as with Path::extension.
    let Some((_, ext)) = lower.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()) else {
        return PLAIN;
    };
    match ext {
        // seti-markdown is a bare arrow at this size.
        "md" | "markdown" | "mdx" => ('\u{f48a}', hex(0x519aba)), // oct-markdown
        "txt" | "rst" => ('\u{f0219}', MUTED),                   // md-file_document
        "log" => ('\u{f4ed}', MUTED),                            // oct-log
        "rs" => ('\u{e68b}', hex(0xdea584)),                     // seti-rust
        "ts" | "mts" | "cts" => ('\u{e628}', hex(0x519aba)),     // seti-typescript
        "js" | "mjs" | "cjs" => ('\u{e60c}', hex(0xcbcb41)),     // seti-javascript
        "tsx" | "jsx" => ('\u{e625}', hex(0x20c2e3)),            // seti-react
        "py" | "pyi" => ('\u{e606}', hex(0xffbc03)),             // seti-python
        "go" => ('\u{e627}', hex(0x00add8)),                     // seti-go
        "rb" => ('\u{e605}', hex(0xe0574f)),                     // seti-ruby
        "java" => ('\u{e66d}', hex(0xcc3e44)),                   // seti-java
        "kt" | "kts" => ('\u{e634}', hex(0x7f52ff)),             // seti-kotlin
        "swift" => ('\u{e699}', hex(0xe37933)),                  // seti-swift
        "c" => ('\u{e649}', hex(0x599eff)),                      // seti-c
        "h" | "hpp" | "hh" | "hxx" => ('\u{e649}', hex(0xa074c4)), // seti-c
        "cpp" | "cc" | "cxx" => ('\u{e646}', hex(0x519aba)),     // seti-cpp
        "cs" => ('\u{f031b}', hex(0x9b4f96)),                    // md-language_csharp
        "php" => ('\u{e608}', hex(0xa074c4)),                    // seti-php
        "lua" => ('\u{e620}', hex(0x51a0cf)),                    // seti-lua
        "zig" => ('\u{e6a9}', hex(0xf69a1b)),                    // seti-zig
        "ex" | "exs" | "heex" => ('\u{e62d}', hex(0xa074c4)),    // seti-elixir
        "hs" => ('\u{e61f}', hex(0xa074c4)),                     // seti-haskell
        "scala" | "sc" => ('\u{e68e}', hex(0xcc3e44)),           // seti-scala
        "dart" => ('\u{e64c}', hex(0x3fa9e8)),                   // seti-dart
        "jl" => ('\u{e624}', hex(0xa270ba)),                     // seti-julia
        "ml" | "mli" => ('\u{e67a}', hex(0xe37933)),             // seti-ocaml
        "clj" | "cljs" | "edn" => ('\u{e642}', hex(0x8dc149)),   // seti-clojure
        "pl" | "pm" => ('\u{e67e}', hex(0x519aba)),              // seti-perl
        "r" => ('\u{e68a}', hex(0x3a8be0)),                      // seti-r
        "elm" => ('\u{e62c}', hex(0x519aba)),                    // seti-elm
        "v" => ('\u{e6ac}', hex(0x5d87bf)),                      // custom-v_lang
        "vim" => ('\u{e62b}', hex(0x3fae4a)),                    // custom-vim
        "tex" | "bib" => ('\u{e69b}', hex(0x8fbf4f)),            // seti-tex
        "sh" | "bash" | "zsh" | "fish" => ('\u{e691}', hex(0x89e051)), // seti-shell
        "ps1" | "psm1" => ('\u{e683}', hex(0x4273ca)),           // seti-powershell
        "json" | "jsonc" | "json5" => ('\u{e60b}', hex(0xcbcb41)), // seti-json
        "toml" => ('\u{e6b2}', hex(0xe8894f)),                   // custom-toml
        "yaml" | "yml" => ('\u{e6a8}', hex(0xa074c4)),           // seti-yml
        "ini" | "conf" | "cfg" => ('\u{e615}', MUTED),           // seti-config
        "xml" | "plist" => ('\u{e619}', hex(0xe37933)),          // seti-xml
        "html" | "htm" => ('\u{e60e}', hex(0xe44d26)),           // seti-html
        "css" => ('\u{e614}', hex(0x42a5f5)),                    // seti-css
        "scss" | "sass" => ('\u{e603}', hex(0xf55385)),          // seti-sass
        "vue" => ('\u{e6a0}', hex(0x8dc149)),                    // seti-vue
        "svelte" => ('\u{e697}', hex(0xff3e00)),                 // seti-svelte
        "jinja" | "j2" => ('\u{e66f}', hex(0xd0bf41)),           // seti-jinja
        "sql" | "sqlite" | "db" => ('\u{e64d}', hex(0xdad8d8)),  // seti-db
        "graphql" | "gql" => ('\u{e662}', hex(0xe535ab)),        // seti-graphql
        "proto" => ('\u{f0fd8}', hex(0x8b74f0)),                 // md-protocol
        "tf" | "tfvars" | "hcl" => ('\u{e69a}', hex(0x8b74f0)),  // seti-terraform
        "nix" => ('\u{f1105}', hex(0x7ebae4)),                   // md-nix
        "wasm" | "wat" => ('\u{e6a1}', hex(0x8070ff)),           // seti-wasm
        "diff" | "patch" => ('\u{e65d}', hex(0xf14c28)),         // seti-git
        "svg" => ('\u{e698}', hex(0xffb13b)),                    // seti-svg
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "icns" | "avif" | "heic"
        | "tiff" => ('\u{e60d}', hex(0xa074c4)), // seti-image
        "pdf" => ('\u{e67d}', hex(0xe05050)),                    // seti-pdf
        "doc" | "docx" | "odt" | "rtf" | "pages" => ('\u{e6a5}', hex(0x4a8ae8)), // seti-word
        "xls" | "xlsx" | "ods" | "numbers" => ('\u{e6a6}', hex(0x4caf72)), // seti-xls
        "csv" | "tsv" => ('\u{e64a}', hex(0x89e051)),            // seti-csv
        "ppt" | "pptx" | "key" => ('\u{f0227}', hex(0xcb4a32)),  // md-file_powerpoint
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" => {
            ('\u{e6aa}', hex(0xeca517)) // seti-zip
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => ('\u{e638}', hex(0x66d8ef)), // seti-audio
        "mp4" | "mov" | "mkv" | "webm" | "avi" => ('\u{e69f}', hex(0xfd971f)), // seti-video
        "ttf" | "otf" | "woff" | "woff2" => ('\u{e659}', hex(0xececec)), // seti-font
        "lock" => ('\u{e672}', hex(0xbbbbbb)),                   // seti-lock
        _ => PLAIN,
    }
}

/// Draws `name`'s icon centred vertically on `left.y`, starting at `left.x`.
pub fn file(p: &Painter, left: Pos2, name: &str) {
    let (glyph, color) = icon(name);
    glyph_at(p, left, glyph, color);
}

pub fn folder(p: &Painter, left: Pos2, open: bool) {
    // fa-folder_open, fa-folder
    let glyph = if open { '\u{f07c}' } else { '\u{f07b}' };
    glyph_at(p, left, glyph, ACCENT);
}

fn glyph_at(p: &Painter, left: Pos2, glyph: char, color: Color32) {
    p.text(
        pos2(left.x + WIDTH / 2.0, left.y),
        Align2::CENTER_CENTER,
        glyph,
        theme::icon_font(14.0),
        color,
    );
}
