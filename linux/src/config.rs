use std::path::Path;
use std::time::SystemTime;

use drv_i18n::Fail;
use gestus::config::{LIMIT, LoadError};
use gestus::{Config, Template};

pub use gestus::config::symbols_beside;

pub const SYSTEM: &str = "/etc/elantp/gestures.toml";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Loaded {
    pub config: Config,
    pub symbols: Vec<Template>,
}

pub fn failure(error: LoadError) -> Fail {
    match error {
        LoadError::Unreadable(path, error) => Fail::new("dev-config-unreadable").arg("file", path.display()).arg("detail", error),
        LoadError::TooBig(path) => Fail::new("dev-config-too-big").arg("file", path.display()).num("limit", LIMIT),
        LoadError::Invalid(path, problem) => {
            Fail::new("dev-config-invalid").arg("file", path.display()).num("line", problem.line).num("column", problem.column).arg("detail", problem.message)
        }
    }
}

pub fn load(path: &Path) -> Result<Loaded, Fail> {
    let (config, symbols) = gestus::config::load(path).map_err(failure)?;
    Ok(Loaded { config, symbols })
}

pub fn stamp(path: &Path) -> Option<(SystemTime, Option<SystemTime>)> {
    let changed = |path: &Path| std::fs::metadata(path).and_then(|meta| meta.modified()).ok();
    Some((changed(path)?, changed(&symbols_beside(path))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_to_the_settings_or_the_symbols_beside_them_changes_the_stamp() {
        let dir = std::env::temp_dir().join(format!("elantp-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gestures.toml");
        assert!(stamp(&path).is_none());
        std::fs::write(&path, "").unwrap();
        let before = stamp(&path);
        assert!(before.is_some_and(|(_, symbols)| symbols.is_none()));
        std::fs::write(symbols_beside(&path), "").unwrap();
        assert!(stamp(&path).is_some_and(|(_, symbols)| symbols.is_some()));
        assert!(load(&path).unwrap().config == Config::default());
        std::fs::write(&path, "[taps]\none = \"jump\"\n").unwrap();
        let fail = load(&path).unwrap_err();
        assert_eq!(fail.0.key, "dev-config-invalid");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_example_settings_shipped_with_the_driver_are_valid() {
        let config = gestus::config::parse(include_str!("../../config/gestures.toml")).unwrap();
        assert!(config.native_extras() && config.corners.enabled && config.symbols.enabled);
    }
}
