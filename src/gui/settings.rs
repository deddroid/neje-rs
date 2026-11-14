//! Settings of the application (not of the engraver), kept in
//! `<config dir>/neje/settings.toml`.

use std::path::PathBuf;

use neje::i18n::Lang;
use neje::protocol::BURN_DEFAULT;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Two-letter language code; `None` follows the locale.
    pub language: Option<String>,
    /// Last burn time used.
    pub burn: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { language: None, burn: BURN_DEFAULT }
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

    pub fn lang(&self) -> Lang {
        self.language.as_deref().and_then(Lang::parse).unwrap_or_else(Lang::from_env)
    }
}
