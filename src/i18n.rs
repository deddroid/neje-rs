//! English by default, Italian selectable. Messages are written in place as
//! `tr("English", "Italiano")`, so the two texts stay next to each other.

use std::sync::atomic::{AtomicBool, Ordering};

static ITALIAN: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    It,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::It];

    /// Name of the language in the language itself.
    pub fn label(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::It => "Italiano",
        }
    }

    /// Two-letter code, as stored in the settings file.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::It => "it",
        }
    }

    /// Parses a code or a locale name such as `it_IT.UTF-8`.
    pub fn parse(s: &str) -> Option<Lang> {
        let s = s.to_ascii_lowercase();
        if s.starts_with("it") {
            Some(Lang::It)
        } else if s.starts_with("en") || s == "c" || s == "posix" {
            Some(Lang::En)
        } else {
            None
        }
    }

    /// Language from `NEJE_LANG`, then from the locale (`LC_ALL`, `LC_MESSAGES`, `LANG`).
    pub fn from_env() -> Lang {
        ["NEJE_LANG", "LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| !value.is_empty())
            .and_then(|value| Lang::parse(&value))
            .unwrap_or(Lang::En)
    }
}

pub fn set(lang: Lang) {
    ITALIAN.store(lang == Lang::It, Ordering::Relaxed);
}

pub fn current() -> Lang {
    if ITALIAN.load(Ordering::Relaxed) { Lang::It } else { Lang::En }
}

/// The text in the current language.
pub fn tr(en: &'static str, it: &'static str) -> &'static str {
    match current() {
        Lang::En => en,
        Lang::It => it,
    }
}
