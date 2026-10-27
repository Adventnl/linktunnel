//! Message catalogue for every user-facing string (English and Simplified Chinese).
//!
//! Catalogues are flat JSON objects (`locales/*.json`) embedded at compile time. A parity test
//! guarantees that every key exists in every locale with the same `{placeholders}`, so a
//! string can never ship untranslated. Lookups fall back to English, then to the key itself,
//! so a missing translation degrades gracefully instead of failing.

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Supported locales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Locale {
    /// English (default and fallback).
    #[default]
    En,
    /// Simplified Chinese.
    ZhCn,
}

impl Locale {
    /// All shipped locales.
    pub const ALL: [Self; 2] = [Self::En, Self::ZhCn];

    /// BCP-47 tag.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
        }
    }

    /// Parses `en`, `en_US.UTF-8`, `zh`, `zh-CN`, `zh_Hans_CN`, … (case-insensitive).
    #[must_use]
    pub fn parse(tag: &str) -> Option<Self> {
        let lower = tag.trim().to_ascii_lowercase().replace('_', "-");
        let lang = lower.split(['.', '@']).next().unwrap_or_default();
        match lang.split('-').next().unwrap_or_default() {
            "en" => Some(Self::En),
            "zh" => {
                // Traditional-script tags are not shipped; do not mislabel them as Simplified.
                let traditional = lang.contains("hant")
                    || lang.ends_with("-tw")
                    || lang.ends_with("-hk")
                    || lang.ends_with("-mo");
                (!traditional).then_some(Self::ZhCn)
            }
            _ => None,
        }
    }

    /// Detects the locale from `NEXUS_LANG`, `LC_ALL`, `LC_MESSAGES`, then `LANG`.
    #[must_use]
    pub fn detect() -> Self {
        ["NEXUS_LANG", "LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .find_map(|v| Self::parse(&v))
            .unwrap_or_default()
    }
}

type Table = BTreeMap<String, String>;

struct Catalog {
    en: Table,
    zh_cn: Table,
}

fn parse_table(json: &str) -> Table {
    serde_json::from_str(json).unwrap_or_default()
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| Catalog {
        en: parse_table(include_str!("../locales/en.json")),
        zh_cn: parse_table(include_str!("../locales/zh-CN.json")),
    })
}

fn table(locale: Locale) -> &'static Table {
    match locale {
        Locale::En => &catalog().en,
        Locale::ZhCn => &catalog().zh_cn,
    }
}

/// Looks up `key` in `locale`, falling back to English and then to the key itself.
#[must_use]
pub fn tr(locale: Locale, key: &str) -> String {
    table(locale)
        .get(key)
        .or_else(|| catalog().en.get(key))
        .cloned()
        .unwrap_or_else(|| key.to_owned())
}

/// Like [`tr`], substituting `{name}` placeholders from `args`.
#[must_use]
pub fn tr_args(locale: Locale, key: &str, args: &[(&str, &str)]) -> String {
    let mut text = tr(locale, key);
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn placeholders(text: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            match after.find('}') {
                Some(close) => {
                    found.insert(after[..close].to_owned());
                    rest = &after[close + 1..];
                }
                None => break,
            }
        }
        found
    }

    #[test]
    fn catalogues_parse() {
        assert!(
            !catalog().en.is_empty(),
            "en.json failed to parse or is empty"
        );
        assert!(
            !catalog().zh_cn.is_empty(),
            "zh-CN.json failed to parse or is empty"
        );
    }

    #[test]
    fn locales_have_identical_keys_and_placeholders() {
        let en = &catalog().en;
        let zh = &catalog().zh_cn;
        let en_keys: BTreeSet<_> = en.keys().collect();
        let zh_keys: BTreeSet<_> = zh.keys().collect();
        assert_eq!(en_keys, zh_keys, "locale key sets differ");
        for (key, en_text) in en {
            assert_eq!(
                placeholders(en_text),
                placeholders(&zh[key]),
                "placeholder mismatch for `{key}`"
            );
            assert!(!zh[key].trim().is_empty(), "`{key}` is empty in zh-CN");
        }
    }

    #[test]
    fn lookup_and_fallback() {
        assert_eq!(tr(Locale::En, "app.name"), "linktunnel");
        assert_eq!(tr(Locale::ZhCn, "cli.identity.device_id"), "设备 ID");
        assert_eq!(tr(Locale::En, "no.such.key"), "no.such.key");
    }

    #[test]
    fn placeholder_substitution() {
        let text = tr_args(Locale::ZhCn, "cli.identity.deleted", &[("store", "钥匙串")]);
        assert_eq!(text, "已从 钥匙串 中删除设备身份。");
    }

    #[test]
    fn parse_tags() {
        assert_eq!(Locale::parse("en_US.UTF-8"), Some(Locale::En));
        assert_eq!(Locale::parse("zh_CN.UTF-8"), Some(Locale::ZhCn));
        assert_eq!(Locale::parse("zh-Hans-CN"), Some(Locale::ZhCn));
        assert_eq!(Locale::parse("ZH"), Some(Locale::ZhCn));
        assert_eq!(Locale::parse("zh_TW"), None, "Traditional is not shipped");
        assert_eq!(Locale::parse("zh-Hant"), None);
        assert_eq!(Locale::parse("fr_FR"), None);
        assert_eq!(Locale::parse(""), None);
    }
}
