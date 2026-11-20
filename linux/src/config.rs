use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use drv_i18n::Fail;
use gestus::{Config, Template};

pub const SYSTEM: &str = "/etc/elantp/gestures.toml";
pub const SYMBOLS: &str = "symbols.toml";
const LIMIT: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Loaded {
    pub config: Config,
    pub symbols: Vec<Template>,
}

fn read(path: &Path) -> Result<String, Fail> {
    let unreadable = |detail: String| Fail::new("dev-config-unreadable").arg("file", path.display()).arg("detail", detail);
    let file = std::fs::File::open(path).map_err(|e| unreadable(e.to_string()))?;
    let mut text = String::new();
    file.take(LIMIT + 1).read_to_string(&mut text).map_err(|e| unreadable(e.to_string()))?;
    if text.len() as u64 > LIMIT {
        return Err(Fail::new("dev-config-too-big").arg("file", path.display()).num("limit", LIMIT));
    }
    Ok(text)
}

pub fn symbols_beside(path: &Path) -> PathBuf {
    path.with_file_name(SYMBOLS)
}

pub fn invalid(path: &Path, problem: gestus::config::Problem) -> Fail {
    Fail::new("dev-config-invalid").arg("file", path.display()).num("line", problem.line).num("column", problem.column).arg("detail", problem.message)
}

pub fn load_symbols(path: &Path) -> Result<Vec<Template>, Fail> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    gestus::config::templates(&read(path)?).map_err(|problem| invalid(path, problem))
}

pub fn load(path: &Path) -> Result<Loaded, Fail> {
    let config = gestus::config::parse(&read(path)?).map_err(|problem| invalid(path, problem))?;
    let symbols = load_symbols(&symbols_beside(path))?;
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
    fn the_settings_and_the_learned_symbols_beside_them_are_read_together() {
        let dir = std::env::temp_dir().join(format!("elantp-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gestures.toml");
        std::fs::write(&path, "[inertia]\nenabled = true\n").unwrap();
        let loaded = load(&path).unwrap();
        assert!(loaded.config.inertia.enabled && loaded.symbols.is_empty());
        let saw = Template { name: "saw".into(), points: vec![(0.0, 0.0), (1.0, 1.0)] };
        std::fs::write(symbols_beside(&path), gestus::config::entry(&saw).unwrap()).unwrap();
        assert_eq!(load(&path).unwrap().symbols, [saw]);
        assert!(stamp(&path).is_some_and(|(_, symbols)| symbols.is_some()));
        std::fs::write(&path, "[inertia]\nenabled = 1\n").unwrap();
        assert!(load(&path).is_err());
        std::fs::write(&path, vec![b' '; LIMIT as usize + 2]).unwrap();
        assert!(load(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(load(&path).is_err() && stamp(&path).is_none());
    }
}
