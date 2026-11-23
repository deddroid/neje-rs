//! Settings of the application (not of the engraver), kept in
//! `<config dir>/neje/settings.toml`.

use std::path::PathBuf;

use neje::i18n::Lang;
use neje::protocol::BURN_DEFAULT;
use neje::text::FontSource;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Two-letter language code; `None` follows the locale.
    pub language: Option<String>,
    /// Last burn time used.
    pub burn: u8,
    /// Font of the last text placed; `None` is the built-in font.
    pub font_path: Option<PathBuf>,
    /// Face in `font_path` (collections hold several).
    pub font_index: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { language: None, burn: BURN_DEFAULT, font_path: None, font_index: 0 }
    }
}

impl Settings {
    /// `NEJE_SETTINGS` points to another file, for tests and screenshots.
    fn path() -> Option<PathBuf> {
        std::env::var_os("NEJE_SETTINGS")
            .map(PathBuf::from)
            .or_else(|| dirs::config_dir().map(|dir| dir.join("neje").join("settings.toml")))
    }

    /// The saved settings, or the defaults when the file is missing or unreadable.
    pub fn load() -> Self {
        Self::path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("no configuration folder on this system")?;
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The remembered font, or the built-in one when its file is gone.
    pub fn font_source(&self) -> FontSource {
        match &self.font_path {
            Some(path) if path.is_file() => FontSource::File { path: path.clone(), index: self.font_index },
            _ => FontSource::BuiltIn,
        }
    }

    /// A path that is not UTF-8 cannot be written to TOML: the built-in font is kept instead,
    /// so that the other settings can still be saved.
    pub fn remember_font(&mut self, font: &FontSource) {
        (self.font_path, self.font_index) = match font {
            FontSource::File { path, index } if path.to_str().is_some() => (Some(path.clone()), *index),
            _ => (None, 0),
        };
    }

    pub fn lang(&self) -> Lang {
        self.language.as_deref().and_then(Lang::parse).unwrap_or_else(Lang::from_env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inter() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/Inter-Regular.ttf")
    }

    #[test]
    fn settings_without_a_font_use_the_built_in_one() {
        let settings: Settings = toml::from_str("language = \"it\"\nburn = 40\n").unwrap();
        assert_eq!(settings.burn, 40);
        assert_eq!(settings.font_source(), FontSource::BuiltIn);
    }

    #[test]
    fn a_remembered_font_comes_back_and_built_in_clears_it() {
        let mut settings = Settings::default();
        let font = FontSource::File { path: inter(), index: 0 };
        settings.remember_font(&font);
        assert_eq!(settings.font_source(), font);
        settings.remember_font(&FontSource::BuiltIn);
        assert_eq!(settings.font_path, None);
        assert_eq!(settings.font_source(), FontSource::BuiltIn);
    }

    #[test]
    fn a_remembered_font_survives_saving() {
        let mut settings = Settings::default();
        settings.remember_font(&FontSource::File { path: inter(), index: 2 });
        let back: Settings = toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
        assert_eq!(back.font_path, Some(inter()));
        assert_eq!(back.font_index, 2);
        assert_eq!(back, settings);
    }

    #[cfg(unix)]
    #[test]
    fn a_font_path_that_is_not_utf8_is_not_remembered() {
        use std::os::unix::ffi::OsStrExt;
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(b"/fonts/bad-\xff.ttf"));
        let mut settings = Settings::default();
        settings.remember_font(&FontSource::File { path, index: 1 });
        assert_eq!((settings.font_path.as_ref(), settings.font_index), (None, 0));
        assert!(toml::to_string(&settings).is_ok());
    }

    #[test]
    fn a_missing_font_file_falls_back_to_built_in() {
        let mut settings = Settings::default();
        settings.remember_font(&FontSource::File { path: inter().with_file_name("Gone-Regular.ttf"), index: 0 });
        assert_eq!(settings.font_source(), FontSource::BuiltIn);
    }
}
