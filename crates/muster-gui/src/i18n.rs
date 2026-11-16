//! UI translations. The English text is the key: `tr("Settings")` looks it up
//! in the current language's table and hands back the English when there's no
//! entry, so a string nobody has translated yet still shows up.
//!
//! Tables are `i18n/<code>.toml` next to Cargo.toml, `"English" = "…"` pairs.
//! Placeholders are `{name}` and filled by `trf!`; a translation may move them
//! around but must keep the same set (a test checks).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Zh,
    Ko,
    Ja,
    De,
    Es,
    Fr,
}

impl Lang {
    pub const ALL: [Lang; 7] = [
        Lang::En,
        Lang::Zh,
        Lang::Ko,
        Lang::Ja,
        Lang::De,
        Lang::Es,
        Lang::Fr,
    ];

    /// What config.toml stores.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Zh => "zh",
            Lang::Ko => "ko",
            Lang::Ja => "ja",
            Lang::De => "de",
            Lang::Es => "es",
            Lang::Fr => "fr",
        }
    }

    /// Anything unrecognised is English, so a typo in config.toml doesn't
    /// throw away the rest of [app].
    pub fn from_code(code: &str) -> Lang {
        let code = code.trim().to_ascii_lowercase();
        let code = code.split(['-', '_']).next().unwrap_or("");
        Lang::ALL
            .into_iter()
            .find(|l| l.code() == code)
            .unwrap_or(Lang::En)
    }

    /// Each language in its own name, for the picker.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Zh => "简体中文",
            Lang::Ko => "한국어",
            Lang::Ja => "日本語",
            Lang::De => "Deutsch",
            Lang::Es => "Español",
            Lang::Fr => "Français",
        }
    }

    fn source(self) -> &'static str {
        match self {
            Lang::En => "",
            Lang::Zh => include_str!("../i18n/zh.toml"),
            Lang::Ko => include_str!("../i18n/ko.toml"),
            Lang::Ja => include_str!("../i18n/ja.toml"),
            Lang::De => include_str!("../i18n/de.toml"),
            Lang::Es => include_str!("../i18n/es.toml"),
            Lang::Fr => include_str!("../i18n/fr.toml"),
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Relaxed);
}

pub fn current() -> Lang {
    Lang::ALL[CURRENT.load(Relaxed) as usize]
}

type Table = HashMap<&'static str, &'static str>;

fn parse(src: &'static str) -> Result<Table, String> {
    let map: HashMap<String, String> = toml::from_str(src).map_err(|e| e.to_string())?;
    // Leaked once per language so lookups can hand out &'static str, which is
    // what every egui label wants.
    Ok(map
        .into_iter()
        .map(|(k, v)| (&*k.leak(), &*v.leak()))
        .collect())
}

fn table(lang: Lang) -> &'static Table {
    static TABLES: [OnceLock<Table>; 7] = [const { OnceLock::new() }; 7];
    TABLES[lang as usize].get_or_init(|| parse(lang.source()).unwrap_or_default())
}

/// `en` in the current language. Takes any lifetime so labels held in tables
/// and structs can go through it too; a hit is 'static anyway.
pub fn tr(en: &str) -> &str {
    match current() {
        Lang::En => en,
        lang => table(lang).get(en).copied().unwrap_or(en),
    }
}

/// `tr`, then each `{name}` replaced: `trf!("{n} waiting", n = count)`.
macro_rules! trf {
    ($en:literal $(, $k:ident = $v:expr)* $(,)?) => {{
        #[allow(unused_mut)]
        let mut out = $crate::i18n::tr($en).to_string();
        $( out = out.replace(concat!("{", stringify!($k), "}"), &$v.to_string()); )*
        out
    }};
}
pub(crate) use trf;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(s: &str) -> BTreeSet<&str> {
        let mut out = BTreeSet::new();
        let mut rest = s;
        while let Some(i) = rest.find('{') {
            rest = &rest[i + 1..];
            let Some(j) = rest.find('}') else { break };
            let name = &rest[..j];
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                out.insert(name);
            }
            rest = &rest[j + 1..];
        }
        out
    }

    #[test]
    fn tables_parse_and_keep_placeholders() {
        for lang in Lang::ALL.into_iter().skip(1) {
            let t = parse(lang.source()).unwrap_or_else(|e| panic!("{}.toml: {e}", lang.code()));
            for (en, local) in &t {
                assert_eq!(
                    placeholders(en),
                    placeholders(local),
                    "{}.toml: {en:?} = {local:?}",
                    lang.code()
                );
                assert!(
                    !local.trim().is_empty(),
                    "{}.toml: {en:?} is empty",
                    lang.code()
                );
            }
        }
    }

    #[test]
    fn codes_round_trip() {
        for l in Lang::ALL {
            assert_eq!(Lang::from_code(l.code()), l);
        }
        assert_eq!(Lang::from_code("zh-CN"), Lang::Zh);
        assert_eq!(Lang::from_code("klingon"), Lang::En);
    }

    #[test]
    fn trf_fills_names() {
        set(Lang::En);
        assert_eq!(trf!("{n} of {m}", n = 2, m = "five"), "2 of five");
    }
}
